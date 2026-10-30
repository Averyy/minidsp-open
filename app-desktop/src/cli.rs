//! CLI command implementations.

#[cfg(unix)]
use crate::discover_daemon_unix;
use crate::output;
use crate::{
    BmCommand, ChannelCommand, Cli, CompressorCommand, CrossoverCommand, FirCommand, PeqCommand,
    apply_force_kind, connect, connect_all_local, discover_daemon_tcp, discover_network_devices,
};
use minidsp::client::MiniDspClient;
use minidsp::transport::tcp::TcpTransport;
use minidsp::transport::ws::WsTransport;
use minidsp_daemon::http::json::{DeviceJson, VersionJson};
use minidsp_protocol::rattle::{
    RattleCurrentPhase, RattlePlan, RattleRequest, RattleResponse, RattleRunState, RattleStatus,
};
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
    max_volume_db: Option<f64>,
    http_server: Option<HttpServerConfig>,
    #[serde(default, deserialize_with = "one_or_many")]
    tcp_server: Vec<TcpServerConfig>,
    static_device: Vec<StaticDeviceConfig>,
    ignore_advertisements: bool,
    // Retain the original minidsp-open spelling as an extension.
    advertise: Option<AdvertiseConfig>,
}

fn one_or_many<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Entries<T> {
        Many(Vec<T>),
        One(T),
    }
    Ok(
        match <Entries<T> as serde::Deserialize>::deserialize(deserializer)? {
            Entries::Many(entries) => entries,
            Entries::One(entry) => vec![entry],
        },
    )
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct HttpServerConfig {
    bind_address: Option<String>,
    #[serde(alias = "cors_origin")]
    allowed_origins: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct TcpServerConfig {
    bind_address: Option<String>,
    device_index: Option<usize>,
    device_serial: Option<u32>,
    advertise: Option<AdvertiseConfig>,
}

#[derive(Debug, serde::Deserialize)]
struct StaticDeviceConfig {
    url: String,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(default)]
struct AdvertiseConfig {
    #[serde(alias = "name")]
    hostname: Option<String>,
    ip: Option<String>,
    bind_address: Option<String>,
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
        let client = MiniDspClient::connect(Box::new(
            TcpTransport::connect(&crate::tcp_address(addr)).await?,
        ))
        .await?;
        devices_out.push(device_json_from_info(&client.device_info()));
    } else if let Some(url) = cli.url.as_deref().or(env_url.as_deref()) {
        if let Some(addr) = url.strip_prefix("tcp://") {
            let client = MiniDspClient::connect(Box::new(
                TcpTransport::connect(&crate::tcp_address(addr)).await?,
            ))
            .await?;
            devices_out.push(device_json_from_info(&client.device_info()));
        } else if (url.starts_with("http://") || url.starts_with("ws://")) && !url.ends_with("/ws")
        {
            devices_out.extend(discover_daemon_tcp(url).await?);
        } else if url.starts_with("http://") || url.starts_with("ws://") {
            let client =
                MiniDspClient::connect(Box::new(WsTransport::connect(url, 0).await?)).await?;
            devices_out.push(device_json_from_info(&client.device_info()));
        } else if url.starts_with("usb://") || url.starts_with("usb:") {
            let client = crate::connect_transport_url(url, 0).await?;
            devices_out.push(device_json_from_info(&client.device_info()));
        }
    } else if let Some(url) = cli.daemon_url.as_deref().or(env_daemon_url.as_deref()) {
        devices_out.extend(discover_daemon_tcp(url).await?);
    } else if let Some(path) = cli.usb.as_deref().or(env_usb.as_deref()) {
        let transport = crate::open_usb_selector(path, 0)?;
        let client = MiniDspClient::connect(Box::new(transport)).await?;
        devices_out.push(device_json_from_info(&client.device_info()));
    } else {
        #[cfg(unix)]
        if std::path::Path::new("/tmp/minidsp.sock").exists()
            && let Ok(devices) = discover_daemon_unix("/tmp/minidsp.sock").await
        {
            devices_out.extend(devices);
        }
        if devices_out.is_empty() {
            for client in crate::discover_local_clients().await? {
                devices_out.push(device_json_from_info(&client.device_info()));
            }
        }
    }
    devices_out.sort_by_key(|device| device.version.serial);

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
    let mut config = if let Some(path) = config_path {
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
    if config.tcp_server.is_empty() {
        config.tcp_server.push(TcpServerConfig::default());
    }

    let state = Arc::new(DaemonState::new());
    state.load_settings(config.max_volume_db).await?;

    // Upstream indexes successfully probed devices by serial, not HID enumeration order.
    for (i, client) in crate::discover_local_clients()
        .await
        .unwrap_or_default()
        .into_iter()
        .enumerate()
    {
        tracing::info!(
            "Device {i}: {} (serial {})",
            client.profile().name,
            client.serial()
        );
        state.add_device(i, Arc::new(client)).await;
    }

    if state.device_count().await == 0 {
        tracing::warn!("No MiniDSP devices found — starting server anyway");
    }

    for device in &config.static_device {
        match crate::connect_transport_url(&device.url, 0).await {
            Ok(client) => {
                let id = state
                    .devices
                    .read()
                    .await
                    .keys()
                    .max()
                    .map_or(0, |id| id + 1);
                state.add_device(id, Arc::new(client)).await;
            }
            Err(error) => tracing::warn!("Static device {}: {error}", device.url),
        }
    }

    // Upstream discovers advertised devices unless explicitly disabled.
    if !config.ignore_advertisements {
        let discovery_state = state.clone();
        tokio::spawn(async move {
            loop {
                let devices = tokio::task::spawn_blocking(|| {
                    crate::discover_network_devices(std::time::Duration::from_secs(8))
                        .map_err(|error| error.to_string())
                })
                .await;
                if let Ok(Ok(devices)) = devices {
                    for device in devices {
                        if device.hw_id == 0xff {
                            continue;
                        }
                        // Do not connect to our own beacons, or duplicate USB/static devices.
                        if discovery_state
                            .devices
                            .read()
                            .await
                            .values()
                            .any(|client| client.serial() == device.serial)
                        {
                            continue;
                        }
                        let url = format!("tcp://{}:5333", device.ip);
                        let client = crate::connect_transport_url(&url, 0).await.ok();
                        if let Some(client) = client {
                            let id = discovery_state
                                .devices
                                .read()
                                .await
                                .keys()
                                .max()
                                .map_or(0, |id| id + 1);
                            discovery_state.add_device(id, Arc::new(client)).await;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        });
    }

    let app = routes::router_with_origins(
        state.clone(),
        config
            .http_server
            .as_ref()
            .and_then(|http| http.allowed_origins.as_deref()),
    )?;

    for tcp in &config.tcp_server {
        let tcp_state = state.clone();
        let tcp_addr = tcp
            .bind_address
            .clone()
            .unwrap_or_else(|| "0.0.0.0:5333".into());
        let index = tcp.device_index;
        let serial = tcp.device_serial;
        tokio::spawn(async move {
            if let Err(e) =
                minidsp_daemon::tcp::serve_device(&tcp_addr, tcp_state, index, serial).await
            {
                tracing::error!("TCP bridge error: {e}");
            }
        });

        let advertise = tcp.advertise.as_ref().or(config.advertise.as_ref());
        let name = cli_advertise
            .or_else(|| advertise.and_then(|a| a.hostname.as_deref()))
            .map(str::to_owned);
        let ip = cli_ip
            .or_else(|| advertise.and_then(|a| a.ip.as_deref()))
            .map(str::parse::<std::net::Ipv4Addr>)
            .transpose()?;
        let bind_address = advertise.and_then(|a| a.bind_address.clone());
        let disc_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = minidsp_daemon::discovery::broadcast_loop_for_device(
                disc_state,
                ip,
                name,
                bind_address.as_deref(),
                index,
                serial,
            )
            .await
            {
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
    for tcp in &config.tcp_server {
        println!(
            "  TCP bridge: {}",
            tcp.bind_address.as_deref().unwrap_or("0.0.0.0:5333")
        );
    }
    println!("  mDNS: _http._tcp.local. (minidsp.local:{mdns_port})");
    #[cfg(unix)]
    println!("  Unix socket: /tmp/minidsp.sock");

    let listener = tokio::net::TcpListener::bind(http_bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// Execute commands from a file (minidsp -f <file>).
pub async fn execute_file(path: &str, cli: &Cli) -> Result<(), Box<dyn std::error::Error>> {
    let contents = if path == "-" {
        use std::io::Read;
        let mut contents = String::new();
        std::io::stdin().read_to_string(&mut contents)?;
        contents
    } else {
        std::fs::read_to_string(path)?
    };
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
                if let Err(e) = execute_line(client, line, cli.output.as_deref()).await {
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
        if let Err(e) = execute_line(&client, line, cli.output.as_deref()).await {
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
    output: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;
    let words = shlex::split(line).ok_or("invalid quoting in command file")?;
    if words.is_empty() {
        return Ok(());
    }
    let mut parsed = Cli::try_parse_from(std::iter::once("minidsp".to_string()).chain(words))?;
    parsed.output = output.map(str::to_owned);
    if matches!(
        parsed.command,
        Some(
            crate::Command::Probe { .. }
                | crate::Command::Server { .. }
                | crate::Command::Rattle(_)
        )
    ) {
        return Err("command is not supported in a command file".into());
    }
    crate::execute_command(client, &parsed).await
}

type CrossoverFilterArgs = (Vec<usize>, Vec<usize>, PeqCommand);

fn crossover_filter_args(
    args: &[String],
) -> Result<CrossoverFilterArgs, Box<dyn std::error::Error>> {
    use clap::Parser;
    fn targets(value: &str, count: usize) -> Result<Vec<usize>, Box<dyn std::error::Error>> {
        if value.eq_ignore_ascii_case("all") {
            return Ok((0..count).collect());
        }
        let value: usize = value.parse()?;
        if value >= count {
            return Err("crossover index out of range".into());
        }
        Ok(vec![value])
    }
    if args.len() < 3 {
        return Err("expected crossover <group> <index> <set|bypass|clear>".into());
    }
    let groups = targets(&args[0], 2)?;
    let indices = targets(&args[1], 4)?;
    let parsed = Cli::try_parse_from(
        ["minidsp", "output", "0", "peq", "0"]
            .into_iter()
            .map(str::to_owned)
            .chain(args[2..].iter().cloned()),
    )?;
    match parsed.command {
        Some(crate::Command::Output {
            cmd: ChannelCommand::Peq { cmd, .. },
            ..
        }) => Ok((groups, indices, cmd)),
        _ => Err("invalid crossover command".into()),
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
                            client.bypass_filter(band.addr, false).await?;
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
                        client.bypass_filter(addr, false).await?;
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
                CrossoverCommand::Compatibility(args) => {
                    let (groups, indices, command) = crossover_filter_args(args)?;
                    for group in groups {
                        let base = [xover.lpf_addr, xover.hpf_addr][group];
                        match command {
                            PeqCommand::Set { b0, b1, b2, a1, a2 } => {
                                for index in &indices {
                                    client
                                        .write_biquad(
                                            base + *index as u16 * 5,
                                            &[b0, b1, b2, a1, a2],
                                        )
                                        .await?;
                                }
                            }
                            PeqCommand::Bypass { state } => {
                                client.bypass_filter(base, state).await?
                            }
                            PeqCommand::Clear => {
                                for index in 0..4 {
                                    client
                                        .write_biquad(base + index * 5, &[1.0, 0.0, 0.0, 0.0, 0.0])
                                        .await?;
                                }
                            }
                        }
                    }
                }
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
        ChannelCommand::Compressor {
            ref cmd,
            bypass,
            threshold,
            ratio,
            attack,
            release,
        } => {
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
            if let Some(bypass) = bypass {
                client.set_status_bypass(comp.bypass, bypass).await?;
            }
            for (value, addr) in [
                (threshold, comp.threshold),
                (ratio, comp.ratio),
                (attack, comp.attack),
                (release, comp.release),
            ] {
                if let Some(value) = value {
                    client.write_dsp(addr, value, true).await?;
                }
            }
            match cmd {
                None => {}
                Some(CompressorCommand::Bypass { state }) => {
                    client.set_status_bypass(comp.bypass, *state).await?;
                }
                Some(CompressorCommand::Set {
                    threshold,
                    ratio,
                    attack,
                    release,
                }) => {
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
            value,
            off,
            invert,
        } => {
            if !is_input {
                return Err("routing only available on input channels".into());
            }
            let table = client
                .profile()
                .dsp_table
                .ok_or("device does not support routing")?;
            let routing = table.routing;
            if channel >= routing.rows || output >= routing.cols {
                return Err("routing index out of range".into());
            }
            let cell = channel as u16 * routing.cols as u16 + output as u16;
            match gain_db.as_str() {
                "gain" => {
                    client
                        .set_channel_gain(
                            routing.gain_base + cell,
                            value.ok_or("missing routing gain")?.parse()?,
                        )
                        .await?
                }
                "enable" => {
                    client
                        .set_channel_mute(
                            routing.status_base + cell,
                            !crate::parse_on_off(&value.ok_or("missing routing state")?)?,
                        )
                        .await?
                }
                _ => {
                    client
                        .set_routing(channel, output, !off, gain_db.parse()?)
                        .await?
                }
            }
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

    // minidsp-rs 8e987f9, minidsp/src/bin/minidsp/handlers.rs:240-243:
    // PEQ clear writes identity and removes bypass. Exercise the command-file path.
    #[tokio::test]
    async fn command_file_clear_enables_identity_filter_and_accepts_crossover() {
        use minidsp::transport::{Transport, TransportError};
        use std::{
            collections::VecDeque,
            future::Future,
            pin::Pin,
            sync::{Arc, Mutex},
        };
        struct ScriptTransport {
            responses: Mutex<VecDeque<Vec<u8>>>,
            sent: Arc<Mutex<Vec<Vec<u8>>>>,
        }
        impl Transport for ScriptTransport {
            fn send_frame(
                &self,
                payload: &[u8],
            ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
                self.sent.lock().unwrap().push(payload.to_vec());
                Box::pin(async { Ok(()) })
            }
            fn recv_frame(
                &self,
                _: u64,
            ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>>
            {
                Box::pin(async {
                    Ok(self
                        .responses
                        .lock()
                        .unwrap()
                        .pop_front()
                        .unwrap_or_else(|| vec![1]))
                })
            }
            fn is_connected(&self) -> bool {
                true
            }
            fn close(
                &self,
            ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
                Box::pin(async { Ok(()) })
            }
            fn url(&self) -> String {
                "usb:compatibility-test".into()
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let client = MiniDspClient::connect(Box::new(ScriptTransport {
            responses: Mutex::new(VecDeque::from([
                vec![0x31, 1, 13, 10],
                vec![5, 100],
                vec![5, 0x30, 0x39],
                vec![5, 0, 0, 40, 0],
            ])),
            sent: sent.clone(),
        }))
        .await
        .unwrap();
        execute_line(&client, "input 0 peq 0 clear", None)
            .await
            .unwrap();
        assert_eq!(sent.lock().unwrap().last().unwrap()[..2], [0x19, 0x00]);
        execute_line(&client, "output 0 crossover 1 2 set -- 1 0 0 0 0", None)
            .await
            .unwrap();
        assert_eq!(sent.lock().unwrap().last().unwrap()[0], 0x30);
        execute_line(&client, "output 0 crossover 1 2 bypass off", None)
            .await
            .unwrap();
        assert_eq!(sent.lock().unwrap().last().unwrap()[..2], [0x19, 0x00]);
    }

    // minidsp-rs 8e987f9, daemon/src/config.rs:5-22, 49-84.
    #[test]
    fn upstream_config_arrays_selectors_and_unknown_keys() {
        let config: DaemonConfig = toml::from_str(
            r#"
ignore_advertisements = true
future_option = 1
[http_server]
bind_address = "127.0.0.1:5381"
allowed_origins = "https://example.com"
[[tcp_server]]
bind_address = "127.0.0.1:5334"
device_index = 1
advertise = { ip = "192.0.2.1", name = "First", bind_address = "0.0.0.0:0" }
[[tcp_server]]
device_serial = 912345
[[static_device]]
url = "tcp://192.0.2.2:5333"
"#,
        )
        .unwrap();
        assert!(config.ignore_advertisements);
        assert_eq!(config.tcp_server.len(), 2);
        assert_eq!(config.tcp_server[0].device_index, Some(1));
        assert_eq!(config.tcp_server[1].device_serial, Some(912345));
        assert_eq!(
            config.tcp_server[0]
                .advertise
                .as_ref()
                .unwrap()
                .hostname
                .as_deref(),
            Some("First")
        );
        assert_eq!(
            config.http_server.unwrap().allowed_origins.as_deref(),
            Some("https://example.com")
        );
        assert_eq!(config.static_device[0].url, "tcp://192.0.2.2:5333");
        assert!(
            toml::from_str::<DaemonConfig>("[tcp_server]\nbind_address = \"127.0.0.1:5333\"")
                .is_ok()
        );
        let config: DaemonConfig =
            toml::from_str("[http_server]\ncors_origin = \"https://ui.example\"").unwrap();
        assert_eq!(
            config.http_server.unwrap().allowed_origins.as_deref(),
            Some("https://ui.example")
        );
    }

    // ezBEQ bd981bf, ezbeq/minidsp.py:1185-1198 emits this crossover syntax;
    // minidsp-rs 8e987f9, minidsp/src/bin/minidsp/main.rs:353-384.
    #[test]
    fn upstream_crossover_and_compressor_arguments() {
        let cli = Cli::try_parse_from([
            "minidsp",
            "output",
            "0",
            "crossover",
            "1",
            "2",
            "set",
            "--",
            "1",
            "0",
            "0",
            "0",
            "0",
        ])
        .unwrap();
        let Some(crate::Command::Output {
            cmd:
                ChannelCommand::Crossover {
                    cmd: CrossoverCommand::Compatibility(args),
                },
            ..
        }) = cli.command
        else {
            panic!("expected crossover");
        };
        let (groups, indices, command) = crossover_filter_args(&args).unwrap();
        assert_eq!(groups, vec![1]);
        assert_eq!(indices, vec![2]);
        assert!(matches!(command, PeqCommand::Set { b0: 1.0, .. }));
        let (_, indices, _) =
            crossover_filter_args(&["all", "all", "bypass", "off"].map(str::to_owned)).unwrap();
        assert_eq!(indices, vec![0, 1, 2, 3]);
        let cli = Cli::try_parse_from([
            "minidsp",
            "output",
            "0",
            "compressor",
            "-b",
            "off",
            "-t",
            "-15",
            "-k",
            "4",
            "-a",
            "10",
            "-r",
            "100",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::Command::Output {
                cmd: ChannelCommand::Compressor {
                    bypass: Some(false),
                    threshold: Some(-15.0),
                    ratio: Some(4.0),
                    attack: Some(10.0),
                    release: Some(100.0),
                    cmd: None
                },
                ..
            })
        ));
        let parsed =
            Cli::try_parse_from(["minidsp", "input", "0", "routing", "1", "-3.5", "--off"])
                .unwrap();
        assert!(matches!(
            parsed.command,
            Some(crate::Command::Input {
                cmd: ChannelCommand::Routing {
                    off: true,
                    value: None,
                    ..
                },
                ..
            })
        ));
        for command in [
            "input 0 routing 1 gain -- -3.5",
            "input 0 routing 1 enable off",
            "input 0 routing 1 -3.5 --off",
        ] {
            assert!(
                Cli::try_parse_from(std::iter::once("minidsp").chain(command.split_whitespace()))
                    .is_ok()
            );
        }
    }

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
