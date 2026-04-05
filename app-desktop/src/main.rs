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
    if let Some(Command::Server { ref bind }) = cli.command {
        return cli::server(bind).await;
    }

    // Connect to device
    let client = connect(&cli).await?;

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
    // 6. Default: try Unix socket, then USB HID

    if let Some(addr) = &cli.tcp {
        let transport = TcpTransport::connect(addr).await?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    if let Some(path) = &cli.usb {
        let transport = HidTransport::open(path)?;
        return Ok(MiniDspClient::connect(Box::new(transport)).await?);
    }

    // Default: try USB HID first (Unix socket TODO)
    match HidTransport::open_first() {
        Ok(transport) => Ok(MiniDspClient::connect(Box::new(transport)).await?),
        Err(e) => Err(format!("no MiniDSP device found: {e}").into()),
    }
}
