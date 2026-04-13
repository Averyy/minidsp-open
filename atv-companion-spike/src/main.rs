mod archive;
mod companion;
mod discovery;
mod matcher;
mod model;
mod scratch;

use std::{
    collections::BTreeSet,
    net::IpAddr,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use archive::enrich_archived_payloads;
use clap::{Parser, Subcommand};
use matcher::{
    MatchRecord, MetadataSample, build_adhoc_sample, load_catalogue, load_debug_samples,
    load_latest_titled_entry, load_sample_file, match_metadata,
};
use model::{
    CredentialsStore, DeviceSelector, DevicesStore, MetadataEntry, NormalizedMetadata, now_millis,
};
use scratch::ScratchPaths;
use serde_json::{Value as JsonValue, json};
use tokio::{net::TcpStream, task::JoinSet, time::timeout};
use tracing::{debug, info};

use crate::{
    companion::{ClientConfig, CompanionClient, CredentialMaterial},
    discovery::scan_for_devices,
};

const CONNECT_REFRESH_SECS: u64 = 2;
const DEFAULT_COMPANION_PORT: u16 = 49153;

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    #[arg(long, default_value = ".scratch/atv-companion")]
    scratch: PathBuf,

    #[arg(long, default_value = "minidsp-open ATV Spike")]
    controller_name: String,

    #[arg(long, short)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Scan {
        #[arg(long, default_value_t = 5)]
        timeout_secs: u64,
    },
    Pair {
        device: Option<String>,
        #[arg(long)]
        pin: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value_t = 49152)]
        port: u16,
    },
    Watch {
        device: Option<String>,
        #[arg(long, default_value_t = 2)]
        poll_secs: u64,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        host: Option<String>,
    },
    Match {
        #[arg(long)]
        catalogue: Option<PathBuf>,
        #[arg(long = "input")]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        all_debug: bool,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        duration_secs: Option<f64>,
        #[arg(long, default_value_t = 900)]
        tolerance_secs: u64,
        #[arg(long, default_value_t = 5)]
        candidate_limit: usize,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.verbose)?;

    let paths = ScratchPaths::new(cli.scratch);
    paths.ensure()?;

    match cli.command {
        Command::Scan { timeout_secs } => run_scan(&paths, timeout_secs).await,
        Command::Pair {
            device,
            pin,
            host,
            port,
        } => run_pair(&paths, &cli.controller_name, device, pin, host, port).await,
        Command::Watch {
            device,
            poll_secs,
            once,
            host,
        } => run_watch(&paths, &cli.controller_name, device, poll_secs, once, host).await,
        Command::Match {
            catalogue,
            inputs,
            all_debug,
            title,
            duration_secs,
            tolerance_secs,
            candidate_limit,
        } => run_match(
            &paths,
            catalogue,
            inputs,
            all_debug,
            title,
            duration_secs,
            tolerance_secs,
            candidate_limit,
        ),
    }
}

fn init_tracing(verbose: bool) -> Result<()> {
    let filter = if verbose { "debug" } else { "info" };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init()
        .map_err(|err| anyhow!("failed to initialize tracing: {err}"))
}

async fn run_scan(paths: &ScratchPaths, timeout_secs: u64) -> Result<()> {
    let store = refresh_devices(paths, timeout_secs)?;

    info!(
        "discovered {} Apple TV companion services",
        store.devices.len()
    );
    for device in store.devices.values() {
        println!(
            "{}  {}:{}  {}",
            device.id, device.connect_host, device.port, device.name
        );
    }

    Ok(())
}

async fn run_pair(
    paths: &ScratchPaths,
    controller_name: &str,
    device: Option<String>,
    pin: Option<String>,
    host: Option<String>,
    port: u16,
) -> Result<()> {
    let mut selected = if let Some(host) = host {
        model::DiscoveredDevice::manual(host, port)
    } else {
        let store = refresh_devices(paths, CONNECT_REFRESH_SECS)?;
        let selector = DeviceSelector::from_optional(device.as_deref());
        store.resolve(selector)?.clone()
    };
    selected.connect_host = choose_connect_host(&selected).await;

    let config = ClientConfig::new(controller_name.to_owned());
    let mut client =
        CompanionClient::connect(&selected.connect_host, selected.port, config).await?;
    let challenge = client
        .start_pairing()
        .await
        .with_context(|| format!("failed to start pairing for {}", selected.id))?;

    println!(
        "Pairing started for {}. If needed, open Settings > Remotes and Devices > Remote App and Devices on the Apple TV.",
        selected.name
    );

    let pin = match pin {
        Some(pin) => pin,
        None => prompt_pin()?,
    };
    if pin.len() != 4 || !pin.chars().all(|ch| ch.is_ascii_digit()) {
        bail!("PIN must be exactly four digits");
    }

    let creds = client
        .finish_pairing(challenge, pin, Some(controller_name.to_owned()))
        .await
        .with_context(|| format!("pairing failed for {}", selected.id))?;
    let stable_device_id = stable_device_id_from_bytes(&creds.atv_id);
    selected.id = stable_device_id.clone();

    let mut store = paths.read_json::<CredentialsStore>(&paths.credentials)?;
    store.by_device.insert(
        stable_device_id.clone(),
        creds.into_record(&stable_device_id),
    );
    paths.write_json_secure(&paths.credentials, &store)?;
    upsert_device_hint(paths, &selected)?;

    println!("paired {}", stable_device_id);
    println!("credentials saved to {}", paths.credentials.display());
    Ok(())
}

async fn run_watch(
    paths: &ScratchPaths,
    controller_name: &str,
    device: Option<String>,
    poll_secs: u64,
    once: bool,
    host: Option<String>,
) -> Result<()> {
    let creds_store = paths.read_json::<CredentialsStore>(&paths.credentials)?;
    let mut devices = paths.read_json::<DevicesStore>(&paths.devices)?;
    if host.is_none() {
        devices = refresh_devices(paths, CONNECT_REFRESH_SECS)?;
    }
    let selector = watch_selector(&creds_store, device.as_deref())?;
    let mut selected = resolve_watched_device(&creds_store, &devices, selector, host.as_deref())?;
    let creds = creds_store
        .by_device
        .get(&selected.id)
        .cloned()
        .ok_or_else(|| anyhow!("no stored credentials for {}", selected.id))?;
    let material = CredentialMaterial::try_from(&creds)?;
    let credential_device_id = creds.device_id.clone();
    selected.connect_host = match host {
        Some(host) => host,
        None => choose_connect_host(&selected).await,
    };
    upsert_device_hint(paths, &selected)?;

    let config = ClientConfig::new(controller_name.to_owned());
    let mut client =
        CompanionClient::connect(&selected.connect_host, selected.port, config).await?;
    client
        .verify(material.clone())
        .await
        .with_context(|| format!("pair-verify failed for {}", credential_device_id))?;
    client
        .perform_initial_handshake(Some(&material.client_id))
        .await?;
    match client.fetch_attention_state().await {
        Ok(attention_state) => {
            write_message_entry(
                paths,
                &selected.id,
                "attention_state_response",
                attention_state.to_json(),
                None,
                Some("FetchAttentionState"),
            )?;
            record_followup_events(
                &mut client,
                paths,
                &selected.id,
                "FetchAttentionState",
                None,
                Duration::from_millis(400),
                Duration::from_millis(200),
            )
            .await?;
        }
        Err(err) => {
            debug!("attention-state fetch failed: {err:#}");
        }
    }
    record_followup_events(
        &mut client,
        paths,
        &selected.id,
        "post_handshake",
        None,
        Duration::from_millis(400),
        Duration::from_millis(200),
    )
    .await?;

    let mut iteration = 0_u64;
    loop {
        iteration += 1;

        let now_playing_started = Instant::now();
        let now_playing = client.fetch_now_playing().await?;
        write_message_entry(
            paths,
            &selected.id,
            "now_playing_response",
            now_playing.to_json(),
            Some(now_playing_started.elapsed()),
            Some("FetchCurrentNowPlayingInfoEvent"),
        )?;
        debug!("wrote now-playing sample for {}", selected.id);
        record_followup_events(
            &mut client,
            paths,
            &selected.id,
            "FetchCurrentNowPlayingInfoEvent",
            Some(now_playing_started),
            Duration::from_secs(4),
            Duration::from_millis(800),
        )
        .await?;

        if iteration == 1 || iteration % 10 == 0 {
            let up_next_started = Instant::now();
            match client.fetch_up_next().await {
                Ok(up_next) => {
                    write_message_entry(
                        paths,
                        &selected.id,
                        "up_next_response",
                        up_next.to_json(),
                        Some(up_next_started.elapsed()),
                        Some("FetchUpNextInfoEvent"),
                    )?;
                    record_followup_events(
                        &mut client,
                        paths,
                        &selected.id,
                        "FetchUpNextInfoEvent",
                        Some(up_next_started),
                        Duration::from_secs(2),
                        Duration::from_millis(500),
                    )
                    .await?;
                }
                Err(err) => {
                    debug!("up-next fetch failed: {err:#}");
                }
            }
        }

        if once {
            break;
        }

        tokio::time::sleep(Duration::from_secs(poll_secs)).await;
    }

    Ok(())
}

fn run_match(
    paths: &ScratchPaths,
    catalogue: Option<PathBuf>,
    inputs: Vec<PathBuf>,
    all_debug: bool,
    title: Option<String>,
    duration_secs: Option<f64>,
    tolerance_secs: u64,
    candidate_limit: usize,
) -> Result<()> {
    let catalogue_path =
        catalogue.unwrap_or_else(|| paths.root.join("catalogue").join("database.json"));
    let catalogue = load_catalogue(&catalogue_path).with_context(|| {
        format!(
            "failed to load BEQ catalogue from {}. download the current snapshot from the upstream ezBEQ source of truth first",
            catalogue_path.display()
        )
    })?;

    let samples = resolve_match_samples(paths, inputs, all_debug, title, duration_secs)?;
    for sample in samples {
        let mut record = match_metadata(&sample.entry.normalized, &catalogue, tolerance_secs);
        record.source_label = sample.source_label.clone();
        paths.append_jsonl(&paths.matches, &record)?;
        print_match_record(&sample, &record, candidate_limit);
    }

    Ok(())
}

fn resolve_match_samples(
    paths: &ScratchPaths,
    inputs: Vec<PathBuf>,
    all_debug: bool,
    title: Option<String>,
    duration_secs: Option<f64>,
) -> Result<Vec<MetadataSample>> {
    match (title, duration_secs) {
        (Some(title), Some(duration_secs)) => {
            if !inputs.is_empty() || all_debug {
                bail!("use either --title/--duration-secs or file-based inputs, not both");
            }
            return Ok(vec![build_adhoc_sample(title, duration_secs)]);
        }
        (Some(_), None) | (None, Some(_)) => {
            bail!("--title and --duration-secs must be provided together");
        }
        (None, None) => {}
    }

    if all_debug {
        if !inputs.is_empty() {
            bail!("use either --all-debug or explicit --input files, not both");
        }
        return load_debug_samples(&paths.debug);
    }

    if !inputs.is_empty() {
        let mut samples = Vec::new();
        for path in inputs {
            samples.push(load_sample_from_path(&path)?);
        }
        return Ok(samples);
    }

    Ok(vec![load_latest_titled_entry(&paths.metadata)?])
}

fn load_sample_from_path(path: &Path) -> Result<MetadataSample> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("jsonl") => load_latest_titled_entry(path),
        Some("json") => load_sample_file(path),
        _ => bail!(
            "unsupported input {} (expected .json or .jsonl)",
            path.display()
        ),
    }
}

fn print_match_record(sample: &MetadataSample, record: &MatchRecord, candidate_limit: usize) {
    println!("source: {}", sample.source_label);
    if let Some(title) = &record.metadata.title {
        println!("  title: {}", title);
    }
    if let Some(duration) = record.metadata.duration {
        println!("  duration_secs: {:.3}", duration);
    }
    if let Some(app) = &record.metadata.app_bundle_id {
        println!("  app_bundle_id: {}", app);
    }
    if let Some(canonical_id) = &record.metadata.canonical_id {
        println!("  canonical_id: {}", canonical_id);
    }
    if let Some(normalized_title) = &record.normalized_title {
        println!("  normalized_title: {}", normalized_title);
    }

    match &record.best {
        Some(best) => {
            let year = best.year.as_deref().unwrap_or("?");
            let runtime = best
                .runtime_minutes
                .map(|value| format!("{value:.0}"))
                .unwrap_or_else(|| "?".to_owned());
            let delta = best
                .duration_delta_seconds
                .map(|value| format!("{value:.0}"))
                .unwrap_or_else(|| "?".to_owned());
            println!(
                "  best_match: {} ({}) runtime={}min delta={}s confidence={:.2}",
                best.title, year, runtime, delta, best.confidence
            );
            println!("  digest: {}", best.digest);
            if record.ambiguous {
                println!("  ambiguous: yes");
                for candidate in record.candidates.iter().take(candidate_limit.max(1)) {
                    let year = candidate.year.as_deref().unwrap_or("?");
                    let delta = candidate
                        .duration_delta_seconds
                        .map(|value| format!("{value:.0}"))
                        .unwrap_or_else(|| "?".to_owned());
                    println!(
                        "    candidate: {} ({}) delta={}s confidence={:.2} digest={}",
                        candidate.title, year, delta, candidate.confidence, candidate.digest
                    );
                }
            } else {
                println!("  ambiguous: no");
            }
        }
        None => {
            println!(
                "  best_match: none within exact-title + {}s window",
                record.tolerance_secs
            );
        }
    }
    println!();
}

fn prompt_pin() -> Result<String> {
    use std::io::{self, Write};

    print!("Enter Apple TV PIN: ");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to read PIN from stdin")?;
    Ok(line.trim().to_owned())
}

fn refresh_devices(paths: &ScratchPaths, timeout_secs: u64) -> Result<DevicesStore> {
    let devices = scan_for_devices(Duration::from_secs(timeout_secs))?;
    let mut store = paths.read_json::<DevicesStore>(&paths.devices)?;
    store.replace(devices);
    paths.write_json(&paths.devices, &store)?;
    Ok(store)
}

fn watch_selector<'a>(
    creds: &'a CredentialsStore,
    device: Option<&'a str>,
) -> Result<DeviceSelector<'a>> {
    Ok(match device {
        Some(target) => DeviceSelector::Selector(target),
        None if creds.by_device.len() == 1 => DeviceSelector::Selector(
            creds
                .by_device
                .keys()
                .next()
                .map(String::as_str)
                .ok_or_else(|| anyhow!("no stored credentials"))?,
        ),
        None => DeviceSelector::Only,
    })
}

fn resolve_watched_device(
    creds: &CredentialsStore,
    devices: &DevicesStore,
    selector: DeviceSelector<'_>,
    host_override: Option<&str>,
) -> Result<model::DiscoveredDevice> {
    let device_id = resolve_credential_device_id(creds, devices, selector)?;

    let mut selected = if let Some(device) = devices.devices.get(&device_id) {
        device.clone()
    } else if let Some(host) = host_override {
        let mut device = model::DiscoveredDevice::manual(host.to_owned(), DEFAULT_COMPANION_PORT);
        device.id = device_id.clone();
        device.name = device_id.clone();
        device
    } else {
        bail!(
            "no cached or discovered device record for {}; rerun scan or use --host",
            device_id
        );
    };

    if let Some(host) = host_override {
        selected.connect_host = host.to_owned();
        if !selected.addresses.iter().any(|address| address == host) {
            selected.addresses.push(host.to_owned());
        }
    }

    Ok(selected)
}

fn resolve_credential_device_id(
    creds: &CredentialsStore,
    devices: &DevicesStore,
    selector: DeviceSelector<'_>,
) -> Result<String> {
    match selector {
        DeviceSelector::Only if creds.by_device.len() == 1 => creds
            .by_device
            .keys()
            .next()
            .cloned()
            .ok_or_else(|| anyhow!("no stored credentials")),
        DeviceSelector::Only if creds.by_device.is_empty() => {
            bail!("no stored credentials; pair first")
        }
        DeviceSelector::Only => {
            if devices.devices.len() == 1 {
                let only = devices
                    .devices
                    .keys()
                    .next()
                    .cloned()
                    .ok_or_else(|| anyhow!("no devices available"))?;
                if creds.by_device.contains_key(&only) {
                    Ok(only)
                } else {
                    bail!("no stored credentials for {}", only)
                }
            } else {
                bail!(
                    "device selector is ambiguous; specify a device ID or use --host with a credential-backed device ID"
                )
            }
        }
        DeviceSelector::Selector(target) => {
            if let Some(id) = credential_id_match(creds, target) {
                return Ok(id);
            }

            let resolved = devices.resolve(DeviceSelector::Selector(target))?;
            if creds.by_device.contains_key(&resolved.id) {
                Ok(resolved.id.clone())
            } else {
                bail!("no stored credentials for {}", resolved.id)
            }
        }
    }
}

fn credential_id_match(creds: &CredentialsStore, target: &str) -> Option<String> {
    if let Some(record) = creds.by_device.get(target) {
        return Some(record.device_id.clone());
    }

    creds
        .by_device
        .values()
        .find(|record| record.device_id.eq_ignore_ascii_case(target))
        .map(|record| record.device_id.clone())
}

fn stable_device_id_from_bytes(bytes: &[u8]) -> String {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .unwrap_or_else(|_| hex::encode(bytes))
}

fn upsert_device_hint(paths: &ScratchPaths, device: &model::DiscoveredDevice) -> Result<()> {
    let mut store = paths.read_json::<DevicesStore>(&paths.devices)?;
    let mut incoming = device.clone();
    incoming.last_seen_ms = now_millis();
    store
        .devices
        .entry(incoming.id.clone())
        .and_modify(|existing| merge_device_hint(existing, &incoming))
        .or_insert(incoming);
    paths.write_json(&paths.devices, &store)
}

fn merge_device_hint(existing: &mut model::DiscoveredDevice, incoming: &model::DiscoveredDevice) {
    existing.last_seen_ms = existing.last_seen_ms.max(incoming.last_seen_ms);
    if !incoming.name.is_empty() {
        existing.name = incoming.name.clone();
    }
    if !incoming.host.is_empty() {
        existing.host = incoming.host.clone();
    }
    if !incoming.connect_host.is_empty() {
        existing.connect_host = incoming.connect_host.clone();
    }
    existing.port = incoming.port;
    for address in &incoming.addresses {
        if !existing.addresses.contains(address) {
            existing.addresses.push(address.clone());
        }
    }
    for (key, value) in &incoming.txt {
        existing.txt.insert(key.clone(), value.clone());
    }
}

async fn choose_connect_host(device: &model::DiscoveredDevice) -> String {
    let candidates = connect_candidates(device);
    if candidates.len() <= 1 {
        return candidates
            .into_iter()
            .next()
            .unwrap_or_else(|| device.connect_host.clone());
    }

    let mut probes = JoinSet::new();
    for host in candidates {
        let port = device.port;
        probes.spawn(async move {
            let started = std::time::Instant::now();
            let outcome = timeout(
                Duration::from_millis(750),
                TcpStream::connect((host.as_str(), port)),
            )
            .await;
            let elapsed = started.elapsed();
            match outcome {
                Ok(Ok(stream)) => {
                    drop(stream);
                    Ok((host, elapsed))
                }
                Ok(Err(err)) => Err((host, format!("connect failed: {err}"))),
                Err(_) => Err((host, "connect timed out".to_owned())),
            }
        });
    }

    let mut best: Option<(String, Duration)> = None;
    while let Some(result) = probes.join_next().await {
        match result {
            Ok(Ok((host, latency))) => {
                debug!(
                    "connect probe for {} via {}:{} succeeded in {} ms",
                    device.id,
                    host,
                    device.port,
                    latency.as_millis()
                );
                if best
                    .as_ref()
                    .map(|(_, best_latency)| latency < *best_latency)
                    .unwrap_or(true)
                {
                    best = Some((host, latency));
                }
            }
            Ok(Err((host, err))) => {
                debug!(
                    "connect probe for {} via {}:{} failed: {}",
                    device.id, host, device.port, err
                );
            }
            Err(err) => {
                debug!("connect probe join error for {}: {}", device.id, err);
            }
        }
    }

    if let Some((host, latency)) = best {
        if host != device.connect_host {
            info!(
                "using {}:{} for {} after {} ms port probe",
                host,
                device.port,
                device.id,
                latency.as_millis()
            );
        }
        host
    } else {
        device.connect_host.clone()
    }
}

fn connect_candidates(device: &model::DiscoveredDevice) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    push_candidate(&mut seen, &mut candidates, device.connect_host.clone());
    for address in &device.addresses {
        if is_probeable_address(address) {
            push_candidate(&mut seen, &mut candidates, address.clone());
        }
    }
    if candidates.is_empty() {
        push_candidate(&mut seen, &mut candidates, device.host.clone());
    }

    candidates
}

fn push_candidate(seen: &mut BTreeSet<String>, candidates: &mut Vec<String>, value: String) {
    if !value.is_empty() && seen.insert(value.clone()) {
        candidates.push(value);
    }
}

fn is_probeable_address(address: &str) -> bool {
    match IpAddr::from_str(address) {
        Ok(IpAddr::V4(ipv4)) => !ipv4.is_loopback(),
        Ok(IpAddr::V6(ipv6)) => !ipv6.is_loopback() && !ipv6.is_unicast_link_local(),
        Err(_) => true,
    }
}

async fn record_followup_events(
    client: &mut CompanionClient,
    paths: &ScratchPaths,
    device_id: &str,
    trigger: &str,
    trigger_started: Option<Instant>,
    initial_wait: Duration,
    idle_grace: Duration,
) -> Result<()> {
    for event in client.collect_events(initial_wait, idle_grace).await? {
        let enriched = enrich_archived_payloads(&event.to_json());
        let kind = classify_message_kind(&enriched, "companion_event");
        let observed_at_ms = now_millis();
        let raw = json!({
            "frame_type": "e_opack",
            "trigger": trigger,
            "message": enriched,
        });
        let mut raw = raw;
        if let Some(trigger_started) = trigger_started {
            raw["trigger_elapsed_ms"] =
                JsonValue::from(trigger_started.elapsed().as_millis() as u64);
        }
        let entry = MetadataEntry {
            device_id: device_id.to_owned(),
            observed_at_ms,
            kind,
            raw: raw.clone(),
            normalized: normalize_metadata(&raw, observed_at_ms),
        };
        paths.append_jsonl(&paths.metadata, &entry)?;
    }

    Ok(())
}

fn write_message_entry(
    paths: &ScratchPaths,
    device_id: &str,
    kind: &str,
    message: JsonValue,
    elapsed: Option<Duration>,
    trigger: Option<&str>,
) -> Result<()> {
    let enriched = enrich_archived_payloads(&message);
    let observed_at_ms = now_millis();
    let mut raw = json!({
        "frame_type": "e_opack",
        "message": enriched,
    });
    if let Some(trigger) = trigger {
        raw["trigger"] = JsonValue::String(trigger.to_owned());
    }
    if let Some(elapsed) = elapsed {
        raw["elapsed_ms"] = JsonValue::from(elapsed.as_millis() as u64);
    }

    let entry = MetadataEntry {
        device_id: device_id.to_owned(),
        observed_at_ms,
        kind: kind.to_owned(),
        raw: raw.clone(),
        normalized: normalize_metadata(&raw, observed_at_ms),
    };
    paths.append_jsonl(&paths.metadata, &entry)
}

fn classify_message_kind(message: &JsonValue, fallback: &str) -> String {
    match message.get("_i").and_then(JsonValue::as_str) {
        Some("NowPlayingInfo") => "now_playing_event".to_owned(),
        Some("SystemStatus") => "system_status_event".to_owned(),
        Some("TVSystemStatus") => "tv_system_status_event".to_owned(),
        Some(other) => format!("companion_event_{}", sanitize_kind(other)),
        None => fallback.to_owned(),
    }
}

fn sanitize_kind(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn normalize_metadata(raw: &JsonValue, observed_at_ms: u64) -> NormalizedMetadata {
    NormalizedMetadata {
        app_bundle_id: find_string(raw, &["bundleIdentifier", "_bundleID", "bundleID"]),
        title: find_string(raw, &["title", "_title", "localizedTitle"]),
        episode_title: find_string(raw, &["episodeTitle", "subtitle", "secondaryTitle"]),
        season_number: find_u64(raw, &["seasonNumber", "season"]),
        episode_number: find_u64(raw, &["episodeNumber", "episode"]),
        duration: find_f64(raw, &["duration", "_duration"]),
        playback_state: find_string(raw, &["playbackState", "state", "playbackStatus"]),
        elapsed_time: find_f64(raw, &["elapsedTime", "position", "elapsed", "timeOffset"]),
        timestamp_ms: Some(observed_at_ms),
        canonical_id: find_string(raw, &["canonicalID", "contentItemIdentifier"]),
        program_id: find_string(raw, &["programID", "programId"]),
        itunes_store_id: find_string(
            raw,
            &[
                "iTunesStoreIdentifier",
                "storeIdentifier",
                "contentIdentifier",
            ],
        ),
        product_page_url: find_string(
            raw,
            &[
                "productURL",
                "productPageURL",
                "showProductPageURL",
                "actionURL",
                "shareURL",
                "showURL",
                "url",
            ],
        ),
        additional_identifier: find_string(
            raw,
            &[
                "externalID",
                "assetIdentifier",
                "channelID",
                "contentID",
                "identifier",
                "showIdentifier",
                "playerIdentifier",
            ],
        ),
    }
}

fn find_string(value: &JsonValue, keys: &[&str]) -> Option<String> {
    match value {
        JsonValue::Object(map) => {
            for (key, value) in map {
                if keys
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(key))
                {
                    if let Some(string) = value.as_str() {
                        return Some(string.to_owned());
                    }
                    if let Some(string) = extract_embedded_string(value) {
                        return Some(string);
                    }
                    if value.is_number() || value.is_boolean() {
                        return Some(value.to_string());
                    }
                }
                if let Some(found) = find_string(value, keys) {
                    return Some(found);
                }
            }
            None
        }
        JsonValue::Array(values) => values.iter().find_map(|item| find_string(item, keys)),
        _ => None,
    }
}

fn extract_embedded_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::Object(map) => map
            .get("NS.relative")
            .and_then(JsonValue::as_str)
            .map(str::to_owned)
            .or_else(|| {
                map.get("url")
                    .and_then(JsonValue::as_str)
                    .map(str::to_owned)
            }),
        _ => None,
    }
}

fn find_u64(value: &JsonValue, keys: &[&str]) -> Option<u64> {
    match value {
        JsonValue::Object(map) => {
            for (key, value) in map {
                if keys
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(key))
                {
                    if let Some(number) = value.as_u64() {
                        return Some(number);
                    }
                    if let Some(number) = value.as_i64() {
                        return u64::try_from(number).ok();
                    }
                    if let Some(number) = value.as_f64() {
                        return Some(number as u64);
                    }
                }
                if let Some(found) = find_u64(value, keys) {
                    return Some(found);
                }
            }
            None
        }
        JsonValue::Array(values) => values.iter().find_map(|item| find_u64(item, keys)),
        _ => None,
    }
}

fn find_f64(value: &JsonValue, keys: &[&str]) -> Option<f64> {
    match value {
        JsonValue::Object(map) => {
            for (key, value) in map {
                if keys
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(key))
                {
                    if let Some(number) = value.as_f64() {
                        return Some(number);
                    }
                    if let Some(number) = value.as_u64() {
                        return Some(number as f64);
                    }
                    if let Some(number) = value.as_i64() {
                        return Some(number as f64);
                    }
                }
                if let Some(found) = find_f64(value, keys) {
                    return Some(found);
                }
            }
            None
        }
        JsonValue::Array(values) => values.iter().find_map(|item| find_f64(item, keys)),
        _ => None,
    }
}

#[allow(dead_code)]
fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_COMPANION_PORT, resolve_watched_device, stable_device_id_from_bytes, watch_selector,
    };
    use crate::model::{
        CredentialRecord, CredentialsStore, DeviceSelector, DevicesStore, DiscoveredDevice,
    };

    #[test]
    fn stable_device_id_prefers_utf8_value() {
        let bytes = b"11111111-2222-4333-8444-555555555555";
        assert_eq!(
            stable_device_id_from_bytes(bytes),
            "11111111-2222-4333-8444-555555555555"
        );
    }

    #[test]
    fn watch_with_host_override_can_use_credentials_without_discovery() {
        let creds = CredentialsStore {
            by_device: [(
                "11111111-2222-4333-8444-555555555555".to_owned(),
                CredentialRecord {
                    device_id: "11111111-2222-4333-8444-555555555555".to_owned(),
                    client_id_hex: String::new(),
                    client_seed_hex: String::new(),
                    client_ltpk_hex: String::new(),
                    atv_id_hex: String::new(),
                    atv_ltpk_hex: String::new(),
                    paired_at_ms: 0,
                },
            )]
            .into_iter()
            .collect(),
        };
        let selector = watch_selector(&creds, None).expect("selector");
        let selected = resolve_watched_device(
            &creds,
            &DevicesStore::default(),
            selector,
            Some("192.0.2.12"),
        )
        .expect("host override should resolve");

        assert_eq!(selected.id, "11111111-2222-4333-8444-555555555555");
        assert_eq!(selected.connect_host, "192.0.2.12");
        assert_eq!(selected.port, DEFAULT_COMPANION_PORT);
    }

    #[test]
    fn watch_prefers_discovered_device_when_available() {
        let creds = CredentialsStore {
            by_device: [(
                "11111111-2222-4333-8444-555555555555".to_owned(),
                CredentialRecord {
                    device_id: "11111111-2222-4333-8444-555555555555".to_owned(),
                    client_id_hex: String::new(),
                    client_seed_hex: String::new(),
                    client_ltpk_hex: String::new(),
                    atv_id_hex: String::new(),
                    atv_ltpk_hex: String::new(),
                    paired_at_ms: 0,
                },
            )]
            .into_iter()
            .collect(),
        };
        let devices = DevicesStore {
            devices: [(
                "11111111-2222-4333-8444-555555555555".to_owned(),
                DiscoveredDevice {
                    id: "11111111-2222-4333-8444-555555555555".to_owned(),
                    name: "Example TV._companion-link._tcp".to_owned(),
                    host: "Example-TV.local".to_owned(),
                    connect_host: "192.0.2.12".to_owned(),
                    port: 49153,
                    addresses: vec!["192.0.2.12".to_owned()],
                    txt: Default::default(),
                    last_seen_ms: 0,
                },
            )]
            .into_iter()
            .collect(),
        };
        let selected = resolve_watched_device(
            &creds,
            &devices,
            DeviceSelector::Selector("11111111-2222-4333-8444-555555555555"),
            Some("2001:db8::1"),
        )
        .expect("selected device");

        assert_eq!(selected.port, 49153);
        assert_eq!(selected.connect_host, "2001:db8::1");
    }
}
