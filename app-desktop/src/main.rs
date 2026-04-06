use clap::{Parser, Subcommand};
use minidsp::client::MiniDspClient;
use minidsp::transport::hid::HidTransport;
use minidsp::transport::tcp::TcpTransport;
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

    /// Force device serial
    #[arg(long = "force-device", value_name = "SERIAL")]
    force_device: Option<String>,

    /// Force DSP version
    #[arg(long = "force-dsp", value_name = "DSP")]
    force_dsp: Option<String>,

    /// Connect via TCP instead of USB
    #[arg(long, value_name = "ADDR")]
    tcp: Option<String>,

    /// Connect to daemon Unix socket
    #[arg(long = "daemon-sock", value_name = "PATH")]
    daemon_sock: Option<String>,

    /// Connect to daemon HTTP URL
    #[arg(long = "daemon-url", value_name = "URL")]
    daemon_url: Option<String>,

    /// Connect to specific USB device
    #[arg(long, value_name = "PATH")]
    usb: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

/// Map a --force-kind string to (hw_id, dsp_version).
fn resolve_force_kind(kind: &str) -> Option<(u8, u8)> {
    match kind.to_lowercase().as_str() {
        "2x4hd" | "2x4-hd" => Some((10, 100)),
        "ddrc24" | "ddrc-24" => Some((10, 101)),
        "flex" => Some((27, 100)),
        "flex-dl" | "flexdl" => Some((27, 101)),
        "shd" => Some((14, 105)),
        "shd-studio" | "shdstudio" => Some((17, 105)),
        "shd-power" | "shdpower" => Some((21, 105)),
        "flex-eight" | "flexeight" | "flex8" => Some((30, 110)),
        "flex-eight-dl" | "flexeightdl" | "flex8dl" => Some((30, 111)),
        "flex-ht" | "flexht" => Some((31, 115)),
        "flex-htx" | "flexhtx" => Some((32, 115)),
        "4x10hd" | "4x10-hd" => Some((1, 0)),
        "10x10hd" | "10x10-hd" => Some((1, 51)),
        "nanodigi-2x8" | "nanodigi2x8" => Some((2, 54)),
        "2x4" => Some((2, 22)),
        "msharc-4x8" | "minisharc" => Some((4, 0)),
        "ddrc-88bm" | "ddrc88bm" => Some((6, 95)),
        "cdsp-8x12v2" | "cdsp8x12v2" => Some((11, 97)),
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
    },
    /// Set master volume in dB
    Gain {
        /// Volume in dB (negative values require --)
        #[arg(allow_hyphen_values = true)]
        db: f32,
    },
    /// Set mute state
    Mute {
        #[arg(value_parser = parse_on_off)]
        state: bool,
    },
    /// Set input source
    Source {
        name: String,
    },
    /// Switch preset (0-3)
    Config {
        preset: u8,
    },
    /// Set Dirac Live bypass
    Dirac {
        #[arg(value_parser = parse_on_off)]
        state: bool,
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
    /// PEQ filter commands
    Peq {
        /// PEQ band index (0-based)
        index: u8,
        #[command(subcommand)]
        cmd: PeqCommand,
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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let cli = Cli::parse();

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
    if let Some(Command::Server { ref bind, ref config }) = cli.command {
        return cli::server(bind, config.as_deref()).await;
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
        Some(Command::Gain { db }) => {
            client.set_volume_db(db).await?;
        }
        Some(Command::Mute { state }) => {
            client.set_mute(state).await?;
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
        Some(Command::Dirac { state }) => {
            client.set_dirac_bypass(!state).await?;
        }
        Some(Command::Input { channel, cmd }) => {
            cli::channel_command(&client, true, channel, cmd).await?;
        }
        Some(Command::Output { channel, cmd }) => {
            cli::channel_command(&client, false, channel, cmd).await?;
        }
        Some(Command::Probe) | Some(Command::Server { .. }) => unreachable!(),
    }

    Ok(())
}

async fn connect(cli: &Cli) -> Result<MiniDspClient, Box<dyn std::error::Error>> {
    // Connection order per ref-compatibility.md:
    // 1. --daemon-sock  2. --tcp  3. --url  4. --daemon-url  5. --usb
    // 6. Default: try /tmp/minidsp.sock, then USB HID

    // 1. Explicit daemon socket
    if let Some(path) = &cli.daemon_sock {
        let transport = TcpTransport::connect_unix(path).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 2. Explicit TCP
    if let Some(addr) = &cli.tcp {
        let transport = TcpTransport::connect(addr).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 4. Daemon URL (HTTP/WS to remote daemon)
    if let Some(url) = &cli.daemon_url {
        // Connect to daemon's TCP endpoint
        let addr = url.trim_start_matches("http://").trim_start_matches("ws://");
        let transport = TcpTransport::connect(addr).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 5. Explicit USB
    if let Some(path) = &cli.usb {
        let transport = HidTransport::open(path)?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // 6. Default: try Unix socket first, then USB HID
    #[cfg(unix)]
    {
        let sock_path = "/tmp/minidsp.sock";
        if std::path::Path::new(sock_path).exists() {
            match TcpTransport::connect_unix(sock_path).await {
                Ok(transport) => {
                    return Ok(MiniDspClient::connect(Box::new(transport)).await?);
                }
                Err(_) => {
                    // Socket exists but daemon not responding — fall through to USB
                    tracing::debug!("Unix socket at {sock_path} not responding, trying USB");
                }
            }
        }
    }

    match HidTransport::open_first() {
        Ok(transport) => Ok(MiniDspClient::connect(Box::new(transport)).await?),
        Err(e) => Err(format!("no MiniDSP device found: {e}").into()),
    }
}
