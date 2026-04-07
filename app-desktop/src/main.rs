use clap::{Parser, Subcommand};
use minidsp::client::MiniDspClient;
use minidsp::transport::hid::HidTransport;
use minidsp::transport::tcp::TcpTransport;
use minidsp::transport::ws::WsTransport;
use minidsp_protocol::source::Source;

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

    /// Connect to specific USB device
    #[arg(long, value_name = "PATH")]
    usb: Option<String>,

    /// Verbose logging (repeat for more: -v, -vv, -vvv)
    #[arg(short = 'v', long = "verbose", action = clap::ArgAction::Count)]
    verbose: u8,

    /// Set log filter (e.g. "minidsp=debug")
    #[arg(long = "log", value_name = "FILTER", hide = true)]
    log_filter: Option<String>,

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

#[derive(Subcommand)]
enum Command {
    /// Print device status (default if no command given)
    Status,
    /// Enumerate connected devices
    Probe,
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
    Source {
        name: String,
    },
    /// Switch preset (0-3)
    Config {
        preset: u8,
    },
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

#[derive(Subcommand)]
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
}

#[derive(Subcommand)]
enum ChannelCommand {
    /// Set channel gain in dB
    Gain {
        #[arg(allow_hyphen_values = true)]
        db: f32,
    },
    /// Set channel mute
    Mute {
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },
    /// Set channel delay in milliseconds
    Delay {
        ms: f32,
    },
    /// Invert channel polarity
    Invert {
        #[arg(value_parser = parse_on_off)]
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
    /// FIR filter (output channels only)
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
    },
}

#[derive(Subcommand)]
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
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },
}

#[derive(Subcommand)]
enum CompressorCommand {
    /// Set compressor bypass
    Bypass {
        #[arg(value_parser = parse_on_off)]
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

#[derive(Subcommand)]
enum FirCommand {
    /// Set FIR bypass
    Bypass {
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },
    /// Load FIR coefficients from file (one coefficient per line)
    Load {
        /// Path to FIR coefficient file
        file: String,
    },
}

#[derive(Subcommand)]
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
        #[arg(value_parser = parse_on_off)]
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

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Configure logging based on -v flags or --log filter
    let default_level = match cli.verbose {
        0 => tracing::Level::WARN,
        1 => tracing::Level::INFO,
        2 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    let filter = if let Some(ref f) = cli.log_filter {
        tracing_subscriber::EnvFilter::new(f)
    } else {
        tracing_subscriber::EnvFilter::from_default_env()
            .add_directive(default_level.into())
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

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
    if matches!(cli.command, Some(Command::Probe)) {
        return cli::probe(&cli).await;
    }

    // Handle server mode
    if let Some(Command::Server { ref bind, ref config, ref advertise, ref ip }) = cli.command {
        return cli::server(bind, config.as_deref(), advertise.as_deref(), ip.as_deref()).await;
    }

    // Connect to device
    let mut client = connect(&cli).await?;

    // Apply --force-kind override
    if let Some(ref kind) = cli.force_kind {
        if let Some((hw_id, dsp_version)) = resolve_force_kind(kind) {
            let profile = minidsp_protocol::device::probe::identify(hw_id, dsp_version);
            tracing::info!("forcing device kind to {} (hw_id={}, dsp={})", profile.name, hw_id, dsp_version);
            client.override_profile(profile).await;
        } else {
            return Err(format!("unknown device kind '{kind}'").into());
        }
    }

    // Execute command
    match cli.command {
        None | Some(Command::Status) => {
            cli::status(&client, cli.output.as_deref()).await?;
        }
        Some(Command::Gain { db, relative }) => {
            if relative {
                let current = client.get_master_status().await?;
                let new_vol = current.volume_db() + db;
                client.set_volume_db(new_vol).await?;
            } else {
                client.set_volume_db(db).await?;
            }
        }
        Some(Command::Mute { ref state }) => {
            let mute = match state.to_lowercase().as_str() {
                "toggle" => {
                    let current = client.get_master_status().await?;
                    !current.is_muted()
                }
                s => parse_on_off(s).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
            };
            client.set_mute(mute).await?;
        }
        Some(Command::Source { ref name }) => {
            let src = Source::from_str_lowercase(name)
                .ok_or_else(|| format!("unknown source '{name}'"))?;
            let idx = client.profile().sources.index_for_source(src)
                .ok_or_else(|| format!("source '{name}' not available on this device"))?;
            client.set_source(idx).await?;
        }
        Some(Command::Config { preset }) => {
            client.set_config(preset).await?;
        }
        Some(Command::Dirac { ref state }) => {
            let dirac_on = match state.to_lowercase().as_str() {
                "toggle" => {
                    // Read current dirac state and invert
                    let status = client.get_status_summary().await?;
                    !status.master.dirac.unwrap_or(false)
                }
                s => parse_on_off(s).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
            };
            client.set_dirac_bypass(!dirac_on).await?;
        }
        Some(Command::Input { channel, cmd }) => {
            cli::channel_command(&client, true, channel, cmd).await?;
        }
        Some(Command::Output { channel, cmd }) => {
            cli::channel_command(&client, false, channel, cmd).await?;
        }
        Some(Command::Debug { cmd }) => {
            match cmd {
                DebugCommand::Send { hex } => {
                    if hex.len() % 2 != 0 {
                        return Err("hex string must have even length".into());
                    }
                    let bytes: Vec<u8> = hex.as_bytes().chunks(2)
                        .map(|c| {
                            let s = std::str::from_utf8(c).map_err(|_| "invalid UTF-8 in hex")?;
                            u8::from_str_radix(s, 16).map_err(|_| format!("invalid hex byte '{s}'"))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let resp = client.raw_command(bytes, minidsp::queue::Priority::Normal).await?;
                    match resp {
                        minidsp_protocol::commands::Response::Raw(data) => {
                            let hex_str: String = data.iter().map(|b| format!("{b:02x}")).collect();
                            println!("{hex_str}");
                        }
                        other => println!("{other:?}"),
                    }
                }
                DebugCommand::Id => {
                    let hw = client.hw_id();
                    println!("hw_id={}, fw={}.{}, dsp={}, serial={}",
                        hw.hw_id, hw.fw_major, hw.fw_minor,
                        client.dsp_version(), client.serial());
                }
                DebugCommand::DumpFloat { addr, count } => {
                    let count = count.min(14);
                    let values = client.read_floats(addr, count).await?;
                    for (i, v) in values.iter().enumerate() {
                        println!("  [{:04X}+{}] = {v}", addr, i);
                    }
                }
            }
        }
        Some(Command::Probe) | Some(Command::Server { .. }) => unreachable!(),
    }

    Ok(())
}

async fn connect(cli: &Cli) -> Result<MiniDspClient, Box<dyn std::error::Error>> {
    // Environment variable overrides (minidsp-rs compatible)
    let env_url = std::env::var("MINIDSP_URL").ok();
    let env_daemon_url = std::env::var("MINIDSPD_URL").ok();
    let env_sock = std::env::var("MINIDSP_SOCK").ok();
    let env_tcp = std::env::var("MINIDSP_TCP").ok();
    let env_usb = std::env::var("MINIDSP_USB").ok();
    let env_index: Option<usize> = std::env::var("MINIDSP_INDEX").ok()
        .and_then(|s| s.parse().ok());
    // MINIDSP_LOG sets the log filter (overrides -v/--log)
    if let Ok(log_env) = std::env::var("MINIDSP_LOG") {
        if cli.log_filter.is_none() {
            // Can't override the already-initialized subscriber, but we set the env
            // for tracing_subscriber::EnvFilter::from_default_env() to pick up
            // SAFETY: called before any threads are spawned (single-threaded at this point)
            unsafe { std::env::set_var("RUST_LOG", &log_env) };
        }
    }

    // Connection order per ref-compatibility.md:
    // 1. --daemon-sock  2. --tcp  3. --url  4. --daemon-url  5. --usb
    // 6. Default: try /tmp/minidsp.sock, then USB HID

    // Resolve device index from CLI flag or env var (used by WS and USB paths)
    let device_idx = cli.device_index.or(env_index).unwrap_or(0);

    // 1. Explicit daemon socket (or MINIDSP_DAEMON_SOCK env var) — WebSocket over Unix
    if let Some(path) = cli.daemon_sock.as_deref().or(env_sock.as_deref()) {
        #[cfg(unix)]
        {
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
        } else if url.starts_with("usb://") {
            let path = &url["usb://".len()..];
            let transport = HidTransport::open(path)?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        } else if url.starts_with("usb:") {
            let path = &url["usb:".len()..];
            let transport = HidTransport::open(path)?;
            return Ok(MiniDspClient::connect(Box::new(transport)).await?);
        } else {
            return Err(format!("unsupported URL scheme in '{url}' (expected tcp://, usb://, or usb:)").into());
        }
    }

    // 4. Daemon URL (or MINIDSPD_URL env var) — connects via WebSocket to /devices/{id}/ws
    if let Some(url) = cli.daemon_url.as_deref().or(env_daemon_url.as_deref()) {
        let transport = WsTransport::connect(url, device_idx).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 5. Explicit USB (or MINIDSP_USB env var)
    if let Some(path) = cli.usb.as_deref().or(env_usb.as_deref()) {
        let transport = HidTransport::open(path)?;
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
