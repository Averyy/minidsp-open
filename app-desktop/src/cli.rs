//! CLI command implementations.

#[cfg(unix)]
use crate::discover_daemon_unix;
use crate::output;
use crate::{
    BmCommand, ChannelCommand, Cli, CompressorCommand, CrossoverCommand, FirCommand, PeqCommand,
    apply_force_kind, connect, connect_all_local, discover_daemon_tcp, discover_network_devices,
};
use minidsp::client::MiniDspClient;
use minidsp::transport::hid;
use minidsp::transport::tcp::TcpTransport;
use minidsp::transport::ws::WsTransport;
use minidsp_daemon::http::json::{DeviceJson, VersionJson};
use minidsp_protocol::device::probe;
use minidsp_protocol::rattle::{
    RattleCurrentPhase, RattlePlan, RattleRequest, RattleResponse, RattleRunState, RattleStatus,
};
use minidsp_protocol::source::Source;
use minidsp_protocol::status::DeviceInfo;
use tokio::io::AsyncWriteExt;

#[derive(clap::Args, Clone, Debug, Default)]
pub(crate) struct RattleArgs {
    /// Comma-separated tone frequencies in Hz
    #[arg(long, value_delimiter = ',', conflicts_with_all = ["start_hz", "end_hz", "count"])]
    tones: Option<Vec<f32>>,
    /// First logarithmically spaced frequency in Hz
    #[arg(long, requires_all = ["end_hz", "count"])]
    start_hz: Option<f32>,
    /// Last logarithmically spaced frequency in Hz
    #[arg(long, requires_all = ["start_hz", "count"])]
    end_hz: Option<f32>,
    /// Number of logarithmically spaced tones
    #[arg(long, requires_all = ["start_hz", "end_hz"])]
    count: Option<u32>,
    /// Seconds per tone
    #[arg(long)]
    hold: Option<f32>,
    /// Seconds of silence between tones
    #[arg(long)]
    gap: Option<f32>,
    /// Number of cycles through all volumes and tones
    #[arg(long)]
    cycles: Option<u32>,
    /// Comma-separated master volumes in dB, quietest first
    #[arg(long, value_delimiter = ',', allow_hyphen_values = true)]
    volume: Option<Vec<f32>>,
}

impl RattleArgs {
    fn request(&self) -> RattleRequest {
        let defaults = RattleRequest::default();
        RattleRequest {
            frequencies_hz: self.tones.clone(),
            start_hz: self.start_hz,
            end_hz: self.end_hz,
            count: self.count,
            tone_sec: self.hold.unwrap_or(defaults.tone_sec),
            gap_sec: self.gap.unwrap_or(defaults.gap_sec),
            cycles: self.cycles.unwrap_or(defaults.cycles),
            volumes_db: self.volume.clone().unwrap_or(defaults.volumes_db),
            ..defaults
        }
    }
}

/// Use HTTP rather than opening a control transport: the server owns the audio.
pub(crate) async fn rattle(cli: &Cli, args: &RattleArgs) -> Result<(), Box<dyn std::error::Error>> {
    let env_url = std::env::var("MINIDSPD_URL").ok();
    let base = cli.daemon_url.as_deref().or(env_url.as_deref());
    if base.is_none() && cli.daemon_sock.is_none() {
        return Err("rattle requires --daemon-url (or MINIDSPD_URL) pointing to a desktop daemon or ESP32 HTTP API; direct USB/TCP control transports cannot play audio".into());
    }
    let request = args.request();
    RattlePlan::build(request.clone())?;
    let id = cli
        .device_index
        .or(std::env::var("MINIDSP_INDEX")
            .ok()
            .and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    let path = format!("/devices/{id}/rattle");
    let body = serde_json::to_vec(&request)?;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let response = start_rattle_safely(
        rattle_http(cli.daemon_sock.as_deref(), base, "POST", &path, &body),
        &mut interrupt,
        || stop_rattle_and_confirm(cli.daemon_sock.as_deref(), base, &path),
    )
    .await?;
    let Some(response) = response else {
        return Ok(());
    };
    let machine = matches!(cli.output.as_deref(), Some("json" | "jsonline"));
    let mut previous = None;
    loop {
        let bytes = tokio::select! {
            signal = &mut interrupt => {
                signal?;
                stop_rattle_and_confirm(cli.daemon_sock.as_deref(), base, &path).await?;
                return Ok(());
            },
            bytes = rattle_http(cli.daemon_sock.as_deref(), base, "GET", &path, b"") => bytes,
        };
        let status: RattleStatus =
            match bytes.and_then(|bytes| serde_json::from_slice(&bytes).map_err(Into::into)) {
                Ok(status) => status,
                Err(error) => {
                    stop_rattle_and_confirm(cli.daemon_sock.as_deref(), base, &path).await?;
                    return Err(error);
                }
            };
        if machine {
            println!("{}", serde_json::to_string(&status)?);
        } else if let Some(current) = &status.current {
            let key = (current.step, current.phase);
            if previous != Some(key) {
                let marker = match current.phase {
                    RattleCurrentPhase::Tone => "▶",
                    RattleCurrentPhase::Gap => "gap:",
                    RattleCurrentPhase::LeadIn => "starting:",
                };
                println!(
                    "{marker} {}  (step {}/{}, cycle {}/{})",
                    current.label,
                    current.step + 1,
                    current.steps,
                    current.cycle + 1,
                    current.cycles
                );
                previous = Some(key);
            }
        }
        let last = status
            .last
            .as_ref()
            .filter(|run| run.started_at_us == response.started_at_us)
            .ok_or("rattle session was replaced or its status is unavailable")?;
        if !status.playing {
            if !machine {
                println!(
                    "{}",
                    serde_json::to_value(last.state)?
                        .as_str()
                        .unwrap_or("unknown")
                );
                for error in [
                    &last.error,
                    &last.volume_restore_error,
                    &last.source_restore_error,
                ]
                .into_iter()
                .flatten()
                {
                    eprintln!("{error}");
                }
            }
            return if last.state == RattleRunState::Failed {
                Err(last
                    .error
                    .clone()
                    .unwrap_or_else(|| "rattle session failed".into())
                    .into())
            } else {
                Ok(())
            };
        }
        tokio::select! {
            signal = &mut interrupt => {
                signal?;
                stop_rattle_and_confirm(cli.daemon_sock.as_deref(), base, &path).await?;
                return Ok(());
            },
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {},
        }
    }
}

async fn start_rattle_safely<S, I, C, F>(
    start: S,
    interrupt: I,
    mut cleanup: C,
) -> Result<Option<RattleResponse>, Box<dyn std::error::Error>>
where
    S: std::future::Future<Output = Result<Vec<u8>, Box<dyn std::error::Error>>>,
    I: std::future::Future<Output = std::io::Result<()>>,
    C: FnMut() -> F,
    F: std::future::Future<Output = Result<(), Box<dyn std::error::Error>>>,
{
    tokio::pin!(start);
    tokio::pin!(interrupt);
    let result = tokio::select! {
        biased;
        signal = &mut interrupt => {
            signal?;
            // Request stop immediately while the POST is still pending. Wait
            // for it to settle, then stop/confirm again to cover a delayed start.
            let (_, stopped) = tokio::join!(&mut start, cleanup());
            stopped?;
            cleanup().await?;
            return Ok(None);
        },
        result = &mut start => result,
    };
    match result {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(response) => Ok(Some(response)),
            Err(error) => {
                cleanup().await?;
                Err(error.into())
            }
        },
        Err(error) => {
            // A complete HTTP rejection is definitive. Transport failures
            // (including timeouts/truncated replies) can hide a running worker.
            if !error.to_string().starts_with("HTTP/1.1 ") {
                cleanup().await?;
            }
            Err(error)
        }
    }
}

async fn stop_rattle_and_confirm(
    socket: Option<&str>,
    base: Option<&str>,
    path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    stop_rattle_and_confirm_with(path, |method, path| async move {
        rattle_http(socket, base, method, &path, b"").await
    })
    .await
}

async fn stop_rattle_and_confirm_with<F, Fut>(
    path: &str,
    mut http: F,
) -> Result<(), Box<dyn std::error::Error>>
where
    F: FnMut(&'static str, String) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, Box<dyn std::error::Error>>>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
    let confirm = async {
        loop {
            let stopped = http("POST", format!("{path}/stop")).await;
            let accepted = stopped
                .as_ref()
                .map(|_| true)
                .unwrap_or_else(|error| error.to_string().starts_with("HTTP/1.1 409 "));
            if accepted {
                let status = http("GET", path.to_string()).await;
                // ESP32 rattle status has no active run during startup; /play's
                // shared busy flag also covers setup and restoration.
                let play_path = path.trim_end_matches("/rattle").to_string() + "/play";
                let play = http("GET", play_path).await;
                if let (Ok(status), Ok(play)) = (status, play) {
                    let status: RattleStatus = serde_json::from_slice(&status)?;
                    let play: minidsp_protocol::playback::PlaybackStatus =
                        serde_json::from_slice(&play)?;
                    if !status.playing && !play.playing {
                        if let Some(run) = status.last {
                            for error in [
                                run.error,
                                run.volume_restore_error,
                                run.source_restore_error,
                            ]
                            .into_iter()
                            .flatten()
                            {
                                eprintln!("{error}");
                            }
                        }
                        return Ok(());
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    };
    tokio::time::timeout_at(deadline, confirm).await
        .map_err(|_| "could not confirm rattle stopped after 120 s; check the device before leaving it unattended")?
}

async fn rattle_http(
    socket: Option<&str>,
    base: Option<&str>,
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let addr = base
        .map(crate::normalize_http_addr)
        .unwrap_or_else(|| "localhost".into());
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut request = request.into_bytes();
    request.extend_from_slice(body);
    let exchange = async {
        #[cfg(unix)]
        if let Some(socket) = socket {
            let mut stream = tokio::net::UnixStream::connect(socket).await?;
            stream.write_all(&request).await?;
            return crate::read_http_response(&mut stream).await;
        }
        #[cfg(not(unix))]
        if socket.is_some() {
            return Err(std::io::Error::other(
                "Unix sockets are unsupported on this platform",
            ));
        }
        let mut stream = tokio::net::TcpStream::connect(&addr).await?;
        stream.write_all(&request).await?;
        crate::read_http_response(&mut stream).await
    };
    let data = tokio::time::timeout(std::time::Duration::from_secs(30), exchange).await??;
    parse_rattle_http_response(&data)
}

fn parse_rattle_http_response(data: &[u8]) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let end = crate::find_subslice(data, b"\r\n\r\n").ok_or("invalid HTTP response")?;
    let head = std::str::from_utf8(&data[..end])?;
    let raw = &data[end + 4..];
    let body = if crate::header_contains_token(head, "Transfer-Encoding", "chunked") {
        crate::decode_chunked_body(raw)?
    } else {
        raw.to_vec()
    };
    if head
        .lines()
        .next()
        .and_then(|s| s.split_whitespace().nth(1))
        != Some("200")
    {
        return Err(format!(
            "{}: {}",
            head.lines().next().unwrap_or(head),
            String::from_utf8_lossy(&body)
        )
        .into());
    }
    Ok(body)
}

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
    #[serde(alias = "name")]
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

fn device_json_from_info(info: &DeviceInfo) -> DeviceJson {
    DeviceJson {
        url: info.url.clone(),
        version: VersionJson {
            hw_id: info.version.hw_id,
            fw_major: info.version.fw_major,
            fw_minor: info.version.fw_minor,
            dsp_version: info.version.dsp_version,
            serial: info.version.serial,
        },
        product_name: info.product_name.clone(),
    }
}

fn render_probe_devices(
    devices: &[DeviceJson],
    format: Option<&str>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if devices.is_empty() {
        return Ok(false);
    }

    match format {
        Some("jsonline") => {
            for device in devices {
                println!("{}", serde_json::to_string(device)?);
            }
        }
        Some("json") => println!("{}", serde_json::to_string_pretty(devices)?),
        _ => {
            print_discovered_devices(devices);
        }
    }

    Ok(true)
}

/// Print device status.
pub async fn status(
    client: &MiniDspClient,
    format: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
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
    let mut devices_out = Vec::new();
    let env_sock = std::env::var("MINIDSP_SOCK").ok();
    let env_tcp = std::env::var("MINIDSP_TCP").ok();
    let env_url = std::env::var("MINIDSP_URL").ok();
    let env_daemon_url = std::env::var("MINIDSPD_URL").ok();
    let env_usb = std::env::var("MINIDSP_USB").ok();

    if let Some(path) = cli.daemon_sock.as_deref().or(env_sock.as_deref()) {
        #[cfg(unix)]
        {
            devices_out.extend(discover_daemon_unix(path).await?);
        }
    } else if let Some(addr) = cli.tcp.as_deref().or(env_tcp.as_deref()) {
        let client = MiniDspClient::connect(Box::new(TcpTransport::connect(addr).await?)).await?;
        devices_out.push(device_json_from_info(&client.device_info()));
    } else if let Some(url) = cli.url.as_deref().or(env_url.as_deref()) {
        if let Some(addr) = url.strip_prefix("tcp://") {
            let client =
                MiniDspClient::connect(Box::new(TcpTransport::connect(addr).await?)).await?;
            devices_out.push(device_json_from_info(&client.device_info()));
        } else if (url.starts_with("http://") || url.starts_with("ws://")) && !url.ends_with("/ws")
        {
            devices_out.extend(discover_daemon_tcp(url).await?);
        } else if url.starts_with("http://") || url.starts_with("ws://") {
            let client =
                MiniDspClient::connect(Box::new(WsTransport::connect(url, 0).await?)).await?;
            devices_out.push(device_json_from_info(&client.device_info()));
        } else if url.starts_with("usb://") || url.starts_with("usb:") {
            let path = if let Some(rest) = url.strip_prefix("usb://") {
                urlencoding::decode(rest.split('?').next().unwrap_or(rest))?.into_owned()
            } else {
                let rest = &url["usb:".len()..];
                urlencoding::decode(rest.split('?').next().unwrap_or(rest))?.into_owned()
            };
            let transport = hid::HidTransport::open(&path)?;
            let client = MiniDspClient::connect(Box::new(transport)).await?;
            devices_out.push(device_json_from_info(&client.device_info()));
        }
    } else if let Some(url) = cli.daemon_url.as_deref().or(env_daemon_url.as_deref()) {
        devices_out.extend(discover_daemon_tcp(url).await?);
    } else if let Some(path) = cli.usb.as_deref().or(env_usb.as_deref()) {
        let transport = hid::HidTransport::open(path)?;
        let client = MiniDspClient::connect(Box::new(transport)).await?;
        devices_out.push(device_json_from_info(&client.device_info()));
    } else {
        let devices = hid::enumerate()?;
        for (i, dev) in devices.iter().enumerate() {
            match hid::HidTransport::open(&dev.path) {
                Ok(transport) => match MiniDspClient::connect(Box::new(transport)).await {
                    Ok(client) => {
                        devices_out.push(device_json_from_info(&client.device_info()));
                    }
                    Err(e) => {
                        if cli.output.as_deref() != Some("json")
                            && cli.output.as_deref() != Some("jsonline")
                        {
                            println!("{i}: Device at {} (open failed: {e})", dev.path);
                        }
                    }
                },
                Err(e) => {
                    if cli.output.as_deref() != Some("json")
                        && cli.output.as_deref() != Some("jsonline")
                    {
                        println!("{i}: Device at {} (open failed: {e})", dev.path);
                    }
                }
            }
        }
    }

    let printed = render_probe_devices(&devices_out, cli.output.as_deref())?;
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
pub async fn server(
    bind: &str,
    config_path: Option<&str>,
    cli_advertise: Option<&str>,
    cli_ip: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use minidsp_daemon::http::routes;
    use minidsp_daemon::state::DaemonState;
    use std::sync::Arc;

    // Load optional TOML config
    let config = if let Some(path) = config_path {
        let contents = std::fs::read_to_string(path)?;
        toml::from_str::<DaemonConfig>(&contents)?
    } else {
        DaemonConfig::default()
    };

    // Override bind address from config if not provided on CLI
    let http_bind = config
        .http_server
        .as_ref()
        .and_then(|h| h.bind_address.as_deref())
        .unwrap_or(bind);
    let tcp_bind = config
        .tcp_server
        .as_ref()
        .and_then(|t| t.bind_address.as_deref())
        .unwrap_or("0.0.0.0:5333");
    // CLI --advertise overrides config file hostname
    let hostname = cli_advertise.or_else(|| {
        config
            .advertise
            .as_ref()
            .and_then(|a| a.hostname.as_deref())
    });

    let state = Arc::new(DaemonState::new());

    // Auto-discover and connect USB devices
    let devices = hid::enumerate().unwrap_or_default();
    for (i, dev) in devices.iter().enumerate() {
        match hid::HidTransport::open(&dev.path) {
            Ok(transport) => match MiniDspClient::connect(Box::new(transport)).await {
                Ok(client) => {
                    let name = probe::product_name(client.hw_id().hw_id, client.dsp_version());
                    tracing::info!("Device {i}: {name} (serial {})", client.serial());
                    state.add_device(i, Arc::new(client)).await;
                }
                Err(e) => tracing::warn!("Device {i}: connect failed: {e}"),
            },
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

    // Start UDP discovery — runs unconditionally so Device Console can find
    // the daemon even before a USB DSP is attached. The broadcast loop polls
    // DaemonState on every tick and switches between full-device and
    // appliance-mode beacons as devices come and go.
    // CLI --ip overrides config file ip; advertise_hostname is passed through
    // so operators can still force a specific instance name via CLI flag.
    let disc_name = hostname.map(str::to_owned);
    let disc_override_ip: Option<std::net::Ipv4Addr> =
        cli_ip.and_then(|s| s.parse().ok()).or_else(|| {
            config
                .advertise
                .as_ref()
                .and_then(|a| a.ip.as_deref())
                .and_then(|s| s.parse().ok())
        });
    let disc_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = minidsp_daemon::discovery::broadcast_loop_with_name(
            disc_state,
            disc_override_ip,
            disc_name,
        )
        .await
        {
            tracing::error!("UDP discovery error: {e}");
        }
    });

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
                    let _ =
                        std::fs::set_permissions(sock_path, std::fs::Permissions::from_mode(0o766));
                    tracing::info!("Unix socket listening on {sock_path}");
                    if let Err(e) = axum::serve(unix_listener, unix_app.into_make_service()).await {
                        tracing::error!("Unix socket server error: {e}");
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to bind Unix socket at {sock_path}: {e}");
                }
            }
        });
    }

    // Start mDNS advertisement unconditionally — `minidsp.local` should
    // resolve whether or not a DSP is attached so `http://minidsp.local/`
    // hits the daemon immediately after boot.
    let mdns_port: u16 = http_bind
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5380);
    let mdns_suffix = state
        .get_device(0)
        .await
        .map(|client| client.serial().to_string());
    let _mdns_handle = match minidsp_daemon::mdns::advertise(mdns_port, mdns_suffix) {
        Ok(mdns) => {
            tracing::info!("mDNS: advertising minidsp.local on port {mdns_port}");
            Some(mdns)
        }
        Err(e) => {
            tracing::warn!("mDNS advertisement failed: {e}");
            None
        }
    };

    println!("minidsp-open daemon listening on {http_bind}");
    println!("  HTTP API: http://{http_bind}/devices");
    println!("  TCP bridge: {tcp_bind}");
    println!("  mDNS: _http._tcp.local. (minidsp.local:{mdns_port})");
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

    let mut client = connect(cli).await?;
    apply_force_kind(&mut client, cli).await?;

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
async fn execute_line(
    client: &MiniDspClient,
    line: &str,
) -> Result<(), Box<dyn std::error::Error>> {
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
            let idx = client
                .profile()
                .sources
                .index_for_source(src)
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
                        let table = client
                            .profile()
                            .dsp_table
                            .ok_or("device does not support PEQ")?;
                        let peq_slice = if is_input {
                            table
                                .inputs
                                .get(ch as usize)
                                .map(|c| c.peq)
                                .ok_or_else(|| format!("channel {ch} out of range"))?
                        } else {
                            table
                                .outputs
                                .get(ch as usize)
                                .map(|c| c.peq)
                                .ok_or_else(|| format!("channel {ch} out of range"))?
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
                                client
                                    .write_biquad(addr, &[1.0, 0.0, 0.0, 0.0, 0.0])
                                    .await?;
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
                    let samples = client.profile().delay_ms_to_samples(ms);
                    let table = client
                        .profile()
                        .dsp_table
                        .ok_or("device does not support channel delay")?;
                    let ch_def = table
                        .outputs
                        .get(ch as usize)
                        .ok_or_else(|| format!("output channel {ch} out of range"))?;
                    let delay_addr = ch_def
                        .delay
                        .ok_or("output channel does not have delay control")?;
                    client
                        .set_channel_delay_samples(delay_addr, samples)
                        .await?;
                }
                "invert" => {
                    if is_input {
                        return Err("input polarity inversion not supported".into());
                    }
                    let state = parse_on_off_str(parts.get(3).unwrap_or(&"on"))?;
                    let table = client
                        .profile()
                        .dsp_table
                        .ok_or("device does not support polarity inversion")?;
                    let ch_def = table
                        .outputs
                        .get(ch as usize)
                        .ok_or_else(|| format!("output channel {ch} out of range"))?;
                    let polarity = ch_def
                        .polarity
                        .ok_or("output channel does not have polarity inversion")?;
                    client.set_invert(polarity, state).await?;
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

fn get_peq_addr(
    client: &MiniDspClient,
    is_input: bool,
    ch: u8,
    band: u8,
) -> Result<u16, Box<dyn std::error::Error>> {
    let table = client
        .profile()
        .dsp_table
        .ok_or("device does not support PEQ")?;
    let peq_slice = if is_input {
        let ch_def = table
            .inputs
            .get(ch as usize)
            .ok_or_else(|| format!("input channel {ch} out of range"))?;
        ch_def.peq
    } else {
        let ch_def = table
            .outputs
            .get(ch as usize)
            .ok_or_else(|| format!("output channel {ch} out of range"))?;
        ch_def.peq
    };
    let peq = peq_slice
        .get(band as usize)
        .ok_or_else(|| format!("PEQ band {band} out of range"))?;
    Ok(peq.addr)
}

fn get_channel_gain_addr(
    client: &MiniDspClient,
    is_input: bool,
    ch: u8,
) -> Result<u16, Box<dyn std::error::Error>> {
    let table = client
        .profile()
        .dsp_table
        .ok_or("device does not support channel gain")?;
    if is_input {
        let ch_def = table
            .inputs
            .get(ch as usize)
            .ok_or_else(|| format!("input channel {ch} out of range"))?;
        ch_def
            .gain
            .ok_or_else(|| format!("input channel {ch} does not have gain control").into())
    } else {
        let ch_def = table
            .outputs
            .get(ch as usize)
            .ok_or_else(|| format!("output channel {ch} out of range"))?;
        Ok(ch_def.gain)
    }
}

fn get_channel_mute_addr(
    client: &MiniDspClient,
    is_input: bool,
    ch: u8,
) -> Result<u16, Box<dyn std::error::Error>> {
    let table = client
        .profile()
        .dsp_table
        .ok_or("device does not support channel mute")?;
    if is_input {
        let ch_def = table
            .inputs
            .get(ch as usize)
            .ok_or_else(|| format!("input channel {ch} out of range"))?;
        ch_def
            .mute
            .ok_or_else(|| format!("input channel {ch} does not have mute control").into())
    } else {
        let ch_def = table
            .outputs
            .get(ch as usize)
            .ok_or_else(|| format!("output channel {ch} out of range"))?;
        Ok(ch_def.mute)
    }
}

/// Handle `minidsp bm` subcommands.
pub async fn bm_command(
    client: &MiniDspClient,
    cmd: BmCommand,
) -> Result<(), Box<dyn std::error::Error>> {
    if !client.supports_bass_management() {
        return Err("device does not support bass management (HT 113/115 firmware only)".into());
    }
    match cmd {
        BmCommand::Status => {
            let state = client.read_bass_management_state().await?;
            println!("Bass management ({} × {} mixer):", state.rows, state.cols);
            println!("  Channels (gain block):");
            for ch in &state.channels {
                println!(
                    "    [{}] gain={:+.2} dB  mute={}",
                    ch.index,
                    ch.gain_db,
                    if ch.muted { "on" } else { "off" }
                );
            }
            println!("  Mixer cells:");
            for cell in &state.mixer {
                let lfe_marker = if cell.row + 1 == state.rows {
                    " (LFE)"
                } else {
                    ""
                };
                println!(
                    "    R{}{} -> C{}: enable={}  gain={:+.2} dB  invert={}",
                    cell.row,
                    lfe_marker,
                    cell.col,
                    if cell.enabled { "on" } else { "off" },
                    cell.gain_db,
                    if cell.polarity_inverted { "on" } else { "off" }
                );
            }
        }
        BmCommand::Channel {
            channel,
            gain,
            mute,
        } => {
            if gain.is_none() && mute.is_none() {
                let cfg = client.read_bm_channel(channel).await?;
                println!(
                    "BM channel {}: gain={:+.2} dB  mute={}",
                    cfg.index,
                    cfg.gain_db,
                    if cfg.muted { "on" } else { "off" }
                );
                return Ok(());
            }
            let patch = minidsp_protocol::status::BmChannelPatch {
                muted: mute,
                gain_db: gain,
            };
            client.set_bm_channel(channel, patch).await?;
        }
        BmCommand::Mixer {
            row,
            col,
            enable,
            gain,
            invert,
        } => {
            if enable.is_none() && gain.is_none() && invert.is_none() {
                let cell = client.read_bm_mixer_cell(row, col).await?;
                println!(
                    "BM mixer R{} -> C{}: enable={}  gain={:+.2} dB  invert={}",
                    cell.row,
                    cell.col,
                    if cell.enabled { "on" } else { "off" },
                    cell.gain_db,
                    if cell.polarity_inverted { "on" } else { "off" }
                );
                return Ok(());
            }
            let patch = minidsp_protocol::status::BmMixerCellPatch {
                enabled: enable,
                gain_db: gain,
                polarity_inverted: invert,
            };
            client.set_bm_mixer_cell(row, col, patch).await?;
        }
    }
    Ok(())
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
            let table = client
                .profile()
                .dsp_table
                .ok_or("device does not support channel delay")?;
            let ch_def = if is_input {
                return Err("input delay not supported".into());
            } else {
                table
                    .outputs
                    .get(channel as usize)
                    .ok_or_else(|| format!("output channel {channel} out of range"))?
            };
            let delay_addr = ch_def
                .delay
                .ok_or("output channel does not have delay control")?;
            let samples = client.profile().delay_ms_to_samples(ms);
            client
                .set_channel_delay_samples(delay_addr, samples)
                .await?;
        }
        ChannelCommand::Invert { state } => {
            let table = client
                .profile()
                .dsp_table
                .ok_or("device does not support polarity inversion")?;
            let ch_def = if is_input {
                return Err("input polarity inversion not supported".into());
            } else {
                table
                    .outputs
                    .get(channel as usize)
                    .ok_or_else(|| format!("output channel {channel} out of range"))?
            };
            let polarity = ch_def
                .polarity
                .ok_or("output channel does not have polarity inversion")?;
            client.set_invert(polarity, state).await?;
        }
        ChannelCommand::Peq { ref index, ref cmd } => {
            if index == "all" {
                // Apply to all PEQ bands on this channel
                let table = client
                    .profile()
                    .dsp_table
                    .ok_or("device does not support PEQ")?;
                let bands = if is_input {
                    table
                        .inputs
                        .get(channel as usize)
                        .map(|ch| ch.peq)
                        .ok_or_else(|| format!("input channel {channel} out of range"))?
                } else {
                    table
                        .outputs
                        .get(channel as usize)
                        .map(|ch| ch.peq)
                        .ok_or_else(|| format!("output channel {channel} out of range"))?
                };
                for band in bands {
                    match cmd {
                        PeqCommand::Set { b0, b1, b2, a1, a2 } => {
                            client
                                .write_biquad(band.addr, &[*b0, *b1, *b2, *a1, *a2])
                                .await?;
                        }
                        PeqCommand::Bypass { state } => {
                            client.bypass_filter(band.addr, *state).await?;
                        }
                        PeqCommand::Clear => {
                            client
                                .write_biquad(band.addr, &[1.0, 0.0, 0.0, 0.0, 0.0])
                                .await?;
                            client.bypass_filter(band.addr, true).await?;
                        }
                    }
                }
            } else {
                let band_idx: u8 = index.parse().map_err(|_| {
                    format!("invalid PEQ index '{index}' (expected number or 'all')")
                })?;
                let addr = get_peq_addr(client, is_input, channel, band_idx)?;
                match cmd {
                    PeqCommand::Set { b0, b1, b2, a1, a2 } => {
                        client
                            .write_biquad(addr, &[*b0, *b1, *b2, *a1, *a2])
                            .await?;
                    }
                    PeqCommand::Bypass { state } => {
                        client.bypass_filter(addr, *state).await?;
                    }
                    PeqCommand::Clear => {
                        client
                            .write_biquad(addr, &[1.0, 0.0, 0.0, 0.0, 0.0])
                            .await?;
                        client.bypass_filter(addr, true).await?;
                    }
                }
            }
        }
        ChannelCommand::Crossover { ref cmd } => {
            if is_input {
                return Err("crossover not available on input channels".into());
            }
            let table = client
                .profile()
                .dsp_table
                .ok_or("device does not support crossover")?;
            let ch_def = table
                .outputs
                .get(channel as usize)
                .ok_or_else(|| format!("output channel {channel} out of range"))?;
            let xover = ch_def
                .crossover
                .ok_or("output channel does not have crossover")?;
            match cmd {
                CrossoverCommand::Set {
                    group,
                    coefficients,
                } => {
                    // Group 0 = LPF, Group 1 = HPF. Each group has 4 biquads (20 coefficients).
                    let base_addr = match *group {
                        0 => xover.lpf_addr,
                        1 => xover.hpf_addr,
                        n => {
                            return Err(format!(
                                "crossover group must be 0 (LPF) or 1 (HPF), got {n}"
                            )
                            .into());
                        }
                    };
                    if coefficients.len() != 20 {
                        return Err(format!(
                            "crossover needs exactly 20 coefficients (4 biquads x 5), got {}",
                            coefficients.len()
                        )
                        .into());
                    }
                    for (i, chunk) in coefficients.chunks(5).enumerate() {
                        let addr = base_addr + (i * 5) as u16;
                        client
                            .write_biquad(addr, &[chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]])
                            .await?;
                    }
                }
                CrossoverCommand::Bypass { state } => {
                    // Crossover bypass is group-level; write the group base only.
                    for base in [xover.lpf_addr, xover.hpf_addr] {
                        client.bypass_filter(base, *state).await?;
                    }
                }
            }
        }
        ChannelCommand::Compressor { ref cmd } => {
            if is_input {
                return Err("compressor not available on input channels".into());
            }
            let table = client
                .profile()
                .dsp_table
                .ok_or("device does not support compressor")?;
            let ch_def = table
                .outputs
                .get(channel as usize)
                .ok_or_else(|| format!("output channel {channel} out of range"))?;
            let comp = ch_def
                .compressor
                .ok_or("output channel does not have compressor")?;
            match cmd {
                CompressorCommand::Bypass { state } => {
                    client.set_status_bypass(comp.bypass, *state).await?;
                }
                CompressorCommand::Set {
                    threshold,
                    ratio,
                    attack,
                    release,
                } => {
                    if *attack <= 0.0 || *release <= 0.0 {
                        return Err("attack and release must be > 0 ms".into());
                    }
                    if *ratio < 1.0 {
                        return Err("ratio must be >= 1.0".into());
                    }
                    // Verify the suspect scalar fields first so a 2x4 HD no-op
                    // fails before we partially apply threshold.
                    client
                        .write_dsp_verified(comp.ratio, *ratio, true, "set compressor ratio")
                        .await?;
                    client
                        .write_dsp_verified(comp.attack, *attack, true, "set compressor attack")
                        .await?;
                    client
                        .write_dsp_verified(comp.release, *release, true, "set compressor release")
                        .await?;
                    client
                        .write_dsp_verified(
                            comp.threshold,
                            *threshold,
                            true,
                            "set compressor threshold",
                        )
                        .await?;
                }
            }
        }
        ChannelCommand::Fir { ref cmd } => {
            let table = client
                .profile()
                .dsp_table
                .ok_or("device does not support FIR")?;
            let fir = if is_input {
                table
                    .inputs
                    .get(channel as usize)
                    .ok_or_else(|| format!("input channel {channel} out of range"))?
                    .fir
                    .ok_or("input channel does not have FIR filter")?
            } else {
                table
                    .outputs
                    .get(channel as usize)
                    .ok_or_else(|| format!("output channel {channel} out of range"))?
                    .fir
                    .ok_or("output channel does not have FIR filter")?
            };
            match cmd {
                FirCommand::Bypass { state } => {
                    client.set_status_bypass(fir.bypass, *state).await?;
                }
                FirCommand::Load { file } => {
                    let contents = std::fs::read_to_string(file)?;
                    let coeffs: Vec<f32> = contents
                        .lines()
                        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
                        .map(|l| l.trim().parse::<f32>())
                        .collect::<Result<Vec<_>, _>>()?;
                    client.load_fir_coefficients(fir, &coeffs).await?;
                }
            }
        }
        ChannelCommand::Routing {
            output,
            gain_db,
            off,
            invert,
        } => {
            if !is_input {
                return Err("routing only available on input channels".into());
            }
            client.set_routing(channel, output, !off, gain_db).await?;
            if let Some(inverted) = invert {
                client
                    .set_routing_polarity(channel, output, inverted)
                    .await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn rattle_arguments_preserve_defaults_and_parse_negative_volume_lists() {
        let cli = Cli::try_parse_from(["minidsp", "rattle"]).unwrap();
        let Some(crate::Command::Rattle(args)) = cli.command else {
            panic!("expected rattle")
        };
        assert_eq!(args.request(), RattleRequest::default());
        let cli = Cli::try_parse_from([
            "minidsp",
            "--daemon-url",
            "http://board:5380",
            "-d",
            "2",
            "-o",
            "jsonline",
            "rattle",
            "--tones",
            "20,25,31.5",
            "--hold",
            "4",
            "--gap",
            "1",
            "--cycles",
            "2",
            "--volume",
            "-40,-30",
        ])
        .unwrap();
        assert_eq!(cli.device_index, Some(2));
        let Some(crate::Command::Rattle(args)) = cli.command else {
            panic!("expected rattle")
        };
        let request = args.request();
        assert_eq!(request.frequencies_hz, Some(vec![20.0, 25.0, 31.5]));
        assert_eq!(request.volumes_db, vec![-40.0, -30.0]);
        assert_eq!(request.cycles, 2);
        assert!(RattlePlan::build(request).is_ok());
    }

    #[test]
    fn rattle_log_spacing_requires_complete_exclusive_arguments() {
        for flags in [
            vec!["--start-hz", "20"],
            vec!["--end-hz", "80"],
            vec!["--count", "3"],
            vec![
                "--tones",
                "40",
                "--start-hz",
                "20",
                "--end-hz",
                "80",
                "--count",
                "3",
            ],
        ] {
            assert!(Cli::try_parse_from(["minidsp", "rattle"].into_iter().chain(flags)).is_err());
        }
        let cli = Cli::try_parse_from([
            "minidsp",
            "rattle",
            "--start-hz",
            "20",
            "--end-hz",
            "80",
            "--count",
            "3",
        ])
        .unwrap();
        let Some(crate::Command::Rattle(args)) = cli.command else {
            panic!("expected rattle")
        };
        let plan = RattlePlan::build(args.request()).unwrap();
        assert_eq!(plan.schedule().frequencies_hz, vec![20.0, 40.0, 80.0]);
    }

    #[tokio::test]
    async fn rattle_stop_retries_startup_conflict_until_shared_busy_flag_is_clear() {
        use std::cell::RefCell;
        let requests = RefCell::new(Vec::new());
        stop_rattle_and_confirm_with("/devices/1/rattle", |method, path| {
            let mut requests = requests.borrow_mut();
            requests.push((method, path.clone()));
            let result = match requests.len() {
                1 => Err("HTTP/1.1 409 Conflict: not started yet".into()),
                2 | 5 => Ok(br#"{"playing":false}"#.to_vec()),
                3 => Ok(br#"{"playing":true}"#.to_vec()),
                4 | 6 => Ok(br#"{"playing":false}"#.to_vec()),
                _ => panic!("confirmation should have finished"),
            };
            async { result }
        })
        .await
        .unwrap();
        assert_eq!(
            &*requests.borrow(),
            &[
                ("POST", "/devices/1/rattle/stop".to_string()),
                ("GET", "/devices/1/rattle".to_string()),
                ("GET", "/devices/1/play".to_string()),
                ("POST", "/devices/1/rattle/stop".to_string()),
                ("GET", "/devices/1/rattle".to_string()),
                ("GET", "/devices/1/play".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn rattle_start_timeout_and_connection_drop_always_stop_and_confirm() {
        use std::cell::Cell;
        for message in [
            "deadline has elapsed",
            "connection reset",
            "invalid HTTP response",
        ] {
            let calls = Cell::new(0);
            let result = start_rattle_safely(
                async { Err(message.into()) },
                std::future::pending(),
                || {
                    calls.set(calls.get() + 1);
                    async { Ok(()) }
                },
            )
            .await;
            assert!(result.is_err());
            assert_eq!(calls.get(), 1);
        }
    }

    #[tokio::test]
    async fn rattle_start_interrupt_stops_before_post_settles_and_again_afterwards() {
        use std::cell::Cell;
        let calls = Cell::new(0);
        let start = async {
            tokio::task::yield_now().await;
            assert!(calls.get() >= 1, "stop must be sent while POST is pending");
            Err("deadline has elapsed".into())
        };
        let result = start_rattle_safely(start, async { Ok(()) }, || {
            calls.set(calls.get() + 1);
            async { Ok(()) }
        })
        .await
        .unwrap();
        assert!(result.is_none());
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn rattle_http_decodes_chunked_status_and_reports_errors() {
        assert_eq!(parse_rattle_http_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n11\r\n{\"playing\":false}\r\n0\r\n\r\n").unwrap(), b"{\"playing\":false}");
        let error =
            parse_rattle_http_response(b"HTTP/1.1 409 Conflict\r\n\r\n{\"error\":\"busy\"}")
                .unwrap_err()
                .to_string();
        assert!(error.contains("409"));
        assert!(error.contains("busy"));
    }

    #[test]
    fn version_clears_ezbeq_htx_input_gate() {
        // ezBEQ enables Flex HTx input PEQs only when the last token of
        // `minidsp -V` is semver > 0.1.12 (see docs/ref-compatibility.md).
        let parse = |version: &str| -> Vec<u64> {
            version
                .split(['+', '-'])
                .next()
                .unwrap()
                .split('.')
                .map(|part| part.parse().unwrap())
                .collect()
        };
        assert!(parse(env!("CARGO_PKG_VERSION")) > parse("0.1.12"));
    }

    #[test]
    fn advertise_config_accepts_upstream_name_and_legacy_hostname() {
        for key in ["name", "hostname"] {
            let config: DaemonConfig = toml::from_str(&format!(
                "[advertise]\n{key} = \"Living room\"\nip = \"192.168.1.2\""
            ))
            .unwrap();
            assert_eq!(
                config.advertise.unwrap().hostname.as_deref(),
                Some("Living room")
            );
        }
    }
}
