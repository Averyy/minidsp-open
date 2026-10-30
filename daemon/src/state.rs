//! Shared daemon state — device registry, broadcast channels.

use minidsp::client::{
    CachedPeqShadow, DeviceShadowState, MiniDspClient, PresetShadowState, SavedPeqState,
};
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::playback::{PlayResponse, PlaybackRunStatus, PlaybackStatus};
use minidsp_protocol::rattle::{
    RattleGenerator, RattleResponse, RattleRunStatus, RattleStatus, elapsed_samples,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tokio::sync::RwLock;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedShadowCache {
    devices: HashMap<u32, DeviceShadowState>,
}

/// Shared daemon state accessible by all HTTP/WS/TCP handlers.
pub struct DaemonState {
    pub settings: Arc<RwLock<minidsp_protocol::settings::Settings>>,
    pub devices: RwLock<HashMap<usize, Arc<MiniDspClient>>>,
    saved_peq: RwLock<HashMap<usize, SavedPeqState>>,
    shadow_cache: RwLock<HashMap<u32, DeviceShadowState>>,
    shadow_cache_path: PathBuf,
    playback_active: AtomicBool,
    last_playback_run: StdMutex<Option<PlaybackRunStatus>>,
    rattle: StdMutex<RattleState>,
    busy_devices: StdMutex<HashSet<usize>>,
}

#[derive(Default)]
struct RattleState {
    active: Option<ActiveRattle>,
    last: Option<RattleRunStatus>,
}

struct ActiveRattle {
    device_id: usize,
    generator: Arc<StdMutex<RattleGenerator>>,
    output_latency_us: Arc<AtomicI64>,
    started: bool,
    audio_finished: bool,
}

impl DaemonState {
    pub fn new() -> Self {
        Self::with_cache_path(shadow_cache_path())
    }

    /// Use an explicit shadow-cache location; settings are stored beside it.
    pub fn with_cache_path(shadow_cache_path: PathBuf) -> Self {
        let shadow_cache = load_shadow_cache(&shadow_cache_path).devices;
        Self {
            settings: Default::default(),
            devices: RwLock::new(HashMap::new()),
            saved_peq: RwLock::new(HashMap::new()),
            shadow_cache: RwLock::new(shadow_cache),
            shadow_cache_path,
            playback_active: AtomicBool::new(false),
            last_playback_run: StdMutex::new(None),
            rattle: StdMutex::new(RattleState::default()),
            busy_devices: StdMutex::new(HashSet::new()),
        }
    }

    pub async fn add_device(&self, id: usize, client: Arc<MiniDspClient>) {
        client.bind_volume_settings(self.settings.clone());
        let epoch = client.reconnect_epoch();
        let enforced = match client
            .enforce_volume_settings(*self.settings.read().await)
            .await
        {
            Ok(()) => Some(epoch),
            Err(err) => {
                // Writes are still clamped; only lowering the current volume is pending.
                tracing::warn!("cannot apply maximum volume to device {id} yet: {err}; retrying");
                None
            }
        };
        watch_volume_ceiling(id, Arc::downgrade(&client), self.settings.clone(), enforced);

        if let Some(saved) = self.saved_peq.read().await.get(&id).cloned() {
            client.set_saved_measurement_state(Some(saved)).await;
        }
        let cached_shadow = self
            .shadow_cache
            .read()
            .await
            .get(&client.serial())
            .cloned();
        if let Some(shadow) = self.prepare_restore_shadow(&client, cached_shadow).await {
            client.apply_shadow_state(&shadow).await;
            let mut cache = self.shadow_cache.write().await;
            cache.insert(client.serial(), shadow);
            if let Err(err) = write_shadow_cache(&self.shadow_cache_path, &cache) {
                tracing::warn!("failed to persist merged device shadow cache: {err}");
            }
        }
        self.devices.write().await.insert(id, client);
    }

    /// Config overrides persisted runtime settings when explicitly supplied.
    pub async fn load_settings(
        &self,
        configured: Option<f64>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let settings = if configured.is_some() {
            minidsp_protocol::settings::Settings::new(configured)?
        } else {
            match fs::read(self.settings_path()) {
                Ok(bytes) => serde_json::from_slice(&bytes)?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Default::default(),
                Err(err) => return Err(err.into()),
            }
        };
        *self.settings.write().await = settings;
        Ok(())
    }

    pub fn persist_settings(
        &self,
        settings: minidsp_protocol::settings::Settings,
    ) -> std::io::Result<()> {
        let path = self.settings_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, serde_json::to_vec_pretty(&settings)?)?;
        fs::rename(temporary, path)
    }

    fn settings_path(&self) -> PathBuf {
        self.shadow_cache_path.with_file_name("settings.json")
    }

    pub async fn remove_device(&self, id: usize) {
        self.devices.write().await.remove(&id);
    }

    pub async fn get_device(&self, id: usize) -> Option<Arc<MiniDspClient>> {
        self.devices.read().await.get(&id).cloned()
    }

    /// Match the upstream TCP binding precedence: serial, index, then first local device.
    pub async fn bridge_device(
        &self,
        index: Option<usize>,
        serial: Option<u32>,
    ) -> Option<Arc<MiniDspClient>> {
        let devices = self.devices.read().await;
        if let Some(serial) = serial {
            devices
                .values()
                .find(|client| client.serial() == serial)
                .cloned()
        } else if let Some(index) = index {
            devices.get(&index).cloned()
        } else {
            devices
                .iter()
                .min_by_key(|(id, client)| (!client.transport_url().starts_with("usb:"), **id))
                .map(|(_, client)| client.clone())
        }
    }

    pub async fn device_count(&self) -> usize {
        self.devices.read().await.len()
    }

    pub async fn set_saved_peq(&self, id: usize, saved: SavedPeqState) {
        self.saved_peq.write().await.insert(id, saved);
    }

    pub async fn get_saved_peq(&self, id: usize) -> Option<SavedPeqState> {
        self.saved_peq.read().await.get(&id).cloned()
    }

    pub async fn clear_saved_peq(&self, id: usize) {
        self.saved_peq.write().await.remove(&id);
    }

    pub async fn persist_device_shadow(&self, client: &MiniDspClient) {
        if let Err(err) = client.refresh_modification_tokens().await {
            tracing::warn!(
                "failed to refresh modification tokens before persisting shadow cache: {err}"
            );
        }
        let shadow = client.export_shadow_state().await;
        let mut cache = self.shadow_cache.write().await;
        cache.insert(client.serial(), shadow);
        if let Err(err) = write_shadow_cache(&self.shadow_cache_path, &cache) {
            tracing::warn!("failed to persist device shadow cache: {err}");
        }
    }

    pub async fn persist_device_shadow_after_preset_mutation(&self, client: &MiniDspClient) {
        if let Err(err) = client.save_active_preset_token().await {
            tracing::warn!(
                "failed to save active preset token before persisting shadow cache: {err}"
            );
        }
        self.persist_device_shadow(client).await;
    }

    // The busy flag only changes under the last-run lock, so a status
    // snapshot never pairs one run's flag with another run's record.
    pub fn try_begin_playback(&self) -> bool {
        let _last_run = self.last_playback_run();
        self.playback_active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn finish_playback(&self) {
        let _last_run = self.last_playback_run();
        self.playback_active.store(false, Ordering::SeqCst);
    }

    pub fn playback_status(&self) -> PlaybackStatus {
        let last_run = self.last_playback_run();
        PlaybackStatus {
            playing: self.playback_active.load(Ordering::SeqCst),
            last: last_run.clone(),
        }
    }

    pub fn record_playback_run_started(&self, response: &PlayResponse) {
        *self.last_playback_run() = Some(PlaybackRunStatus::started(response));
    }

    /// Settle the run recorded by `record_playback_run_started()`.
    pub fn finish_last_playback_run(&self, error: Option<String>) {
        if let Some(run) = self.last_playback_run().as_mut() {
            run.finish(error);
        }
    }

    pub fn set_last_playback_run_source_restore_error(&self, error: String) {
        if let Some(run) = self.last_playback_run().as_mut() {
            run.source_restore_error = Some(error);
        }
    }

    fn last_playback_run(&self) -> std::sync::MutexGuard<'_, Option<PlaybackRunStatus>> {
        self.last_playback_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    pub fn prepare_rattle(
        &self,
        device_id: usize,
        generator: Arc<StdMutex<RattleGenerator>>,
        output_latency_us: Arc<AtomicI64>,
    ) {
        self.rattle.lock().unwrap_or_else(|e| e.into_inner()).active = Some(ActiveRattle {
            device_id,
            generator,
            output_latency_us,
            started: false,
            audio_finished: false,
        });
    }

    pub fn record_rattle_run_started(&self, response: &RattleResponse) {
        let mut rattle = self.rattle.lock().unwrap_or_else(|e| e.into_inner());
        rattle.last = Some(RattleRunStatus::started(response));
        if let Some(active) = &mut rattle.active {
            active.started = true;
        }
    }

    pub fn rattle_status(&self, id: usize) -> RattleStatus {
        let rattle = self.rattle.lock().unwrap_or_else(|e| e.into_inner());
        let active = rattle.active.as_ref().filter(|run| run.device_id == id);
        let current = active
            .filter(|run| run.started && !run.audio_finished)
            .and_then(|active| {
                let run = rattle.last.as_ref()?;
                let sample = elapsed_samples(
                    run.started_at_us,
                    crate::playback::server_time_us(),
                    run.schedule.sample_rate,
                    active.output_latency_us.load(Ordering::SeqCst),
                );
                // A stopped generator can end well before the planned schedule.
                let generator = active.generator.lock().unwrap_or_else(|e| e.into_inner());
                if generator.is_finished() && sample >= generator.cursor() {
                    return None;
                }
                run.schedule.current_at(sample)
            });
        RattleStatus {
            playing: active.is_some(),
            current,
            last: rattle.last.clone(),
        }
    }

    pub fn request_rattle_stop(&self, id: usize) -> bool {
        let rattle = self.rattle.lock().unwrap_or_else(|e| e.into_inner());
        let Some(active) = rattle.active.as_ref().filter(|run| run.device_id == id) else {
            return false;
        };
        active
            .generator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .request_stop();
        true
    }

    pub fn finish_rattle_audio(&self, error: Option<String>) {
        let mut rattle = self.rattle.lock().unwrap_or_else(|e| e.into_inner());
        let Some(active) = &mut rattle.active else {
            return;
        };
        active.audio_finished = true;
        let started = active.started;
        let stopped = active
            .generator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .was_stopped();
        if started && let Some(run) = &mut rattle.last {
            if error.is_none() && stopped {
                run.stopped();
            } else {
                run.finish(error);
            }
        }
    }

    pub fn finish_rattle(&self, volume_error: Option<String>, source_error: Option<String>) {
        let mut rattle = self.rattle.lock().unwrap_or_else(|e| e.into_inner());
        if rattle.active.as_ref().is_some_and(|active| active.started)
            && let Some(run) = &mut rattle.last
        {
            run.set_restore_errors(volume_error, source_error);
        }
        rattle.active = None;
        drop(rattle);
        self.finish_playback();
    }

    pub fn try_begin_device_operation(&self, id: usize) -> bool {
        let mut busy = self.busy_devices.lock().unwrap_or_else(|e| e.into_inner());
        if busy.contains(&id) {
            false
        } else {
            busy.insert(id);
            true
        }
    }

    pub fn finish_device_operation(&self, id: usize) {
        self.busy_devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    pub fn device_operation_active(&self, id: usize) -> bool {
        self.busy_devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
    }

    async fn prepare_restore_shadow(
        &self,
        client: &MiniDspClient,
        cached_shadow: Option<DeviceShadowState>,
    ) -> Option<DeviceShadowState> {
        let mut shadow = cached_shadow.unwrap_or_default();

        let current_tokens = match client.refresh_modification_tokens().await {
            Ok(()) => client.export_shadow_state().await.preset_tokens,
            Err(err) => {
                tracing::warn!("failed to refresh preset tokens before shadow restore: {err}");
                shadow.preset_tokens.clone()
            }
        };

        if !current_tokens.is_empty() {
            shadow = filter_shadow_state_by_tokens(&shadow, &current_tokens);
        }

        let imported = import_official_app_presets(
            &mut shadow,
            client.profile(),
            &client.device_info().product_name,
            client.serial(),
        );
        if imported > 0 {
            tracing::info!(
                "imported {imported} token-matched preset shadow(s) from official Device Console files for serial {}",
                client.serial()
            );
        }

        has_shadow_data(&shadow).then_some(shadow)
    }
}

const VOLUME_CEILING_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// Reapplies the maximum volume after the device's transport reconnects (a
/// restarted device reports its stored volume), and retries a first attempt
/// that failed. Ends when the client is dropped.
fn watch_volume_ceiling(
    id: usize,
    client: std::sync::Weak<MiniDspClient>,
    settings: Arc<RwLock<minidsp_protocol::settings::Settings>>,
    mut enforced: Option<u64>,
) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(VOLUME_CEILING_CHECK).await;
            let Some(client) = client.upgrade() else {
                return;
            };
            let epoch = client.reconnect_epoch();
            if enforced == Some(epoch) {
                continue;
            }
            match client.enforce_volume_settings(*settings.read().await).await {
                Ok(()) => {
                    if enforced.is_none() {
                        tracing::info!("maximum volume applied to device {id}");
                    }
                    enforced = Some(epoch);
                }
                Err(err) => tracing::debug!("device {id}: maximum volume not applied yet: {err}"),
            }
        }
    });
}

impl Default for DaemonState {
    fn default() -> Self {
        Self::new()
    }
}

fn shadow_cache_path() -> PathBuf {
    let mut path = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    path.push(".config");
    path.push("minidsp-open");
    path.push("shadow-cache.json");
    path
}

fn load_shadow_cache(path: &PathBuf) -> PersistedShadowCache {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => PersistedShadowCache::default(),
    }
}

fn write_shadow_cache(
    path: &PathBuf,
    cache: &HashMap<u32, DeviceShadowState>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let payload = PersistedShadowCache {
        devices: cache.clone(),
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(&payload)?)?;
    Ok(())
}

fn has_shadow_data(state: &DeviceShadowState) -> bool {
    !state.presets.is_empty()
        || !state.peq.is_empty()
        || !state.selector.is_empty()
        || !state.scalar.is_empty()
}

fn filter_shadow_state_by_tokens(
    state: &DeviceShadowState,
    current_tokens: &HashMap<u8, [u8; 4]>,
) -> DeviceShadowState {
    if state.preset_tokens.is_empty() {
        let mut unchanged = state.clone();
        unchanged.preset_tokens = current_tokens.clone();
        return unchanged;
    }

    let mut filtered = state.clone();
    filtered
        .presets
        .retain(|slot, _| state.preset_tokens.get(slot) == current_tokens.get(slot));
    filtered
        .trusted_snapshot_presets
        .retain(|slot| state.preset_tokens.get(slot) == current_tokens.get(slot));
    if state.preset_tokens != *current_tokens {
        // Legacy copies have no preset identity and cannot be trusted after
        // any token changes; matching per-preset entries remain available.
        filtered.peq.clear();
        filtered.selector.clear();
        filtered.scalar.clear();
    }
    filtered.preset_tokens = current_tokens.clone();
    filtered
}

fn import_official_app_presets(
    state: &mut DeviceShadowState,
    profile: &DeviceProfile,
    product_name: &str,
    serial: u32,
) -> usize {
    if state.preset_tokens.is_empty() {
        return 0;
    }

    let Some(root) = official_app_settings_root() else {
        return 0;
    };

    let (selector_addrs, scalar_addrs) = official_xml_shadow_addrs(profile);
    if selector_addrs.is_empty() && scalar_addrs.is_empty() {
        return 0;
    }

    let mut xml_files = Vec::new();
    collect_setting_xml_files(&root, &mut xml_files);

    let product_slug = official_product_slug(product_name);
    let serial_text = serial.to_string();
    let serial_suffix = if serial_text.len() > 5 {
        &serial_text[serial_text.len() - 5..]
    } else {
        &serial_text
    };

    let mut by_dir: HashMap<PathBuf, Vec<(u8, PresetShadowState)>> = HashMap::new();
    let mut scores: HashMap<PathBuf, usize> = HashMap::new();

    for path in xml_files {
        let Some(slot) = official_setting_slot(&path) else {
            continue;
        };
        if state.presets.contains_key(&slot) {
            continue;
        }
        let Some((timestamp, preset)) =
            parse_official_setting_xml(&path, &selector_addrs, &scalar_addrs, profile.dsp_version)
        else {
            continue;
        };
        if state.preset_tokens.get(&slot).copied() != Some(timestamp.to_be_bytes()) {
            continue;
        }

        let Some(dir) = path.parent().map(Path::to_path_buf) else {
            continue;
        };
        let entry = by_dir.entry(dir.clone()).or_default();
        entry.push((slot, preset));

        let score = scores.entry(dir.clone()).or_default();
        *score += 100;
        let path_text = dir.to_string_lossy().to_ascii_lowercase();
        if !product_slug.is_empty() && path_text.contains(&product_slug) {
            *score += 10;
        }
        if path_text.contains(&format!("sn{}", serial_text)) {
            *score += 10;
        } else if path_text.contains(&format!("sn{}", serial_suffix)) {
            *score += 5;
        }
    }

    let Some((best_dir, mut presets)) = by_dir
        .into_iter()
        .max_by_key(|(dir, presets)| (scores.get(dir).copied().unwrap_or(0), presets.len()))
    else {
        return 0;
    };

    presets.sort_by_key(|(slot, _)| *slot);

    let mut imported = 0usize;
    for (slot, preset) in presets {
        if state.presets.insert(slot, preset).is_none() {
            imported += 1;
        }
        state.trusted_snapshot_presets.insert(slot);
    }

    if imported > 0 {
        tracing::info!(
            "using official Device Console preset cache at {} for serial {}",
            best_dir.display(),
            serial
        );
    }

    imported
}

fn official_app_settings_root() -> Option<PathBuf> {
    let mut path = PathBuf::from(std::env::var_os("HOME")?);
    path.push("Documents");
    path.push("miniDSP");
    path.push("MiniDSP Device Console");
    path.exists().then_some(path)
}

fn collect_setting_xml_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_setting_xml_files(&path, files);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with("setting") && name.ends_with(".xml") {
            files.push(path);
        }
    }
}

fn official_product_slug(product_name: &str) -> String {
    product_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

fn official_setting_slot(path: &Path) -> Option<u8> {
    let name = path.file_name()?.to_str()?;
    let digits = name
        .strip_prefix("setting")?
        .strip_suffix(".xml")?
        .parse::<u8>()
        .ok()?;
    digits.checked_sub(1)
}

fn official_xml_shadow_addrs(profile: &DeviceProfile) -> (HashSet<u16>, HashSet<u16>) {
    let mut selector = HashSet::new();
    let mut scalar = HashSet::new();
    let Some(table) = profile.dsp_table else {
        return (selector, scalar);
    };

    for input in table.inputs {
        if let Some(mute) = input.mute {
            selector.insert(mute);
        }
        if let Some(gain) = input.gain {
            scalar.insert(gain);
        }
    }

    for output in table.outputs {
        selector.insert(output.mute);
        scalar.insert(output.gain);
        if let Some(delay) = output.delay {
            scalar.insert(delay);
        }
        if let Some(fir) = output.fir {
            selector.insert(fir.bypass);
            selector.insert(fir.taps_count);
        }
        if let Some(comp) = output.compressor {
            selector.insert(comp.bypass);
            scalar.insert(comp.threshold);
            scalar.insert(comp.gain);
            scalar.insert(comp.ratio);
            scalar.insert(comp.knee);
            scalar.insert(comp.attack);
            scalar.insert(comp.release);
        }
    }

    let routing_cells = table.routing.rows as u16 * table.routing.cols as u16;
    for offset in 0..routing_cells {
        selector.insert(table.routing.status_base + offset);
        scalar.insert(table.routing.gain_base + offset);
    }

    (selector, scalar)
}

fn parse_official_setting_xml(
    path: &Path,
    selector_addrs: &HashSet<u16>,
    scalar_addrs: &HashSet<u16>,
    expected_dsp_version: u8,
) -> Option<(u32, PresetShadowState)> {
    let content = fs::read_to_string(path).ok()?;

    // The setting*.xml schema has three first-class record types:
    //   <item addr="N"> ... </item>      selector/scalar addresses
    //   <filter addr="N"> ... </filter>  biquad PEQ (5 coefficient words + bypass)
    //   <fir addr="N"> ... </fir>        raw FIR coefficients (2048+ f32 per block)
    //
    // We parse <item> into selector/scalar buckets per the profile's known
    // address sets. We parse <filter> into the PEQ shadow, keyed by the same
    // biquad base address our dsp.rs tables use. We deliberately DO NOT parse
    // <fir> coefficient payloads — the FIR *tap count* lives in an ordinary
    // <item> and is already captured via the selector bucket; the raw taps are
    // not consumed by any runtime readback path today and would be ~8 KiB per
    // block × 4 blocks × 4 presets if we stored them.
    enum Bucket {
        Selector,
        Scalar,
        Filter,
        Ignore,
    }

    let mut timestamp = None;
    let mut dsp_version = None;
    let mut current_addr: Option<u16> = None;
    let mut current_bucket = Bucket::Ignore;
    let mut current_filter_bypass: Option<bool> = None;
    let mut current_filter_coeffs: Option<[f32; 5]> = None;
    let mut selector = HashMap::new();
    let mut scalar = HashMap::new();
    let mut peq: HashMap<u16, CachedPeqShadow> = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if let Some(text) = extract_tag_text(line, "timestamp") {
            timestamp = text.parse::<u32>().ok();
            continue;
        }
        if let Some(text) = extract_tag_text(line, "dspversion") {
            dsp_version = text.parse::<u8>().ok();
            continue;
        }
        if line.starts_with("<item ") {
            let addr = extract_item_addr(line)?;
            current_addr = Some(addr);
            current_bucket = if selector_addrs.contains(&addr) {
                Bucket::Selector
            } else if scalar_addrs.contains(&addr) {
                Bucket::Scalar
            } else {
                Bucket::Ignore
            };
            continue;
        }
        if line.starts_with("<filter ") {
            let addr = extract_item_addr(line)?;
            current_addr = Some(addr);
            current_bucket = Bucket::Filter;
            current_filter_bypass = None;
            current_filter_coeffs = None;
            continue;
        }
        if line.starts_with("<fir ") {
            // Swallow the block so embedded <hex>/<para>/<subpara> lines don't
            // leak into whatever bucket preceded it. Intentionally no capture.
            current_addr = None;
            current_bucket = Bucket::Ignore;
            continue;
        }
        if matches!(current_bucket, Bucket::Filter) {
            if let Some(text) = extract_tag_text(line, "bypass") {
                current_filter_bypass = text.parse::<u8>().ok().map(|v| v != 0);
                continue;
            }
            if let Some(text) = extract_tag_text(line, "hex") {
                current_filter_coeffs = parse_filter_hex_coeffs(text);
                continue;
            }
            if line.starts_with("</filter>") {
                if let (Some(addr), Some(coeffs)) = (current_addr, current_filter_coeffs) {
                    peq.insert(
                        addr,
                        CachedPeqShadow {
                            coeffs,
                            bypass: current_filter_bypass.unwrap_or(false),
                        },
                    );
                }
                current_addr = None;
                current_bucket = Bucket::Ignore;
                current_filter_bypass = None;
                current_filter_coeffs = None;
            }
            continue;
        }
        if let Some(hex) = extract_tag_text(line, "hex") {
            let Some(addr) = current_addr else {
                continue;
            };
            let value = u32::from_str_radix(hex, 16).ok()?;
            match current_bucket {
                Bucket::Selector => {
                    selector.insert(addr, value as u16);
                }
                Bucket::Scalar => {
                    scalar.insert(addr, value);
                }
                Bucket::Filter | Bucket::Ignore => {}
            }
            continue;
        }
        if line.starts_with("</item>") {
            current_addr = None;
            current_bucket = Bucket::Ignore;
        }
    }

    if dsp_version? != expected_dsp_version {
        return None;
    }

    Some((
        timestamp?,
        PresetShadowState {
            peq,
            selector,
            scalar,
            fir_blocks: HashMap::new(),
        },
    ))
}

/// Parse a `<hex>` line from a `<filter>` block: five comma-separated 32-bit
/// IEEE 754 floats in lowercase hex. Returns None on any short or malformed
/// value so partial writes never poison the shadow.
fn parse_filter_hex_coeffs(text: &str) -> Option<[f32; 5]> {
    let mut coeffs = [0f32; 5];
    let mut iter = text.split(',');
    for slot in coeffs.iter_mut() {
        let word = iter.next()?.trim();
        let bits = u32::from_str_radix(word, 16).ok()?;
        *slot = f32::from_bits(bits);
    }
    Some(coeffs)
}

fn extract_tag_text<'a>(line: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = line.find(&open)? + open.len();
    let end = line[start..].find(&close)? + start;
    Some(&line[start..end])
}

fn extract_item_addr(line: &str) -> Option<u16> {
    let start = line.find("addr=\"")? + 6;
    let end = line[start..].find('"')? + start;
    line[start..end].parse::<u16>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidsp_protocol::device::probe;

    #[test]
    fn filter_shadow_state_keeps_only_matching_presets() {
        let state = DeviceShadowState {
            presets: HashMap::from([
                (0, PresetShadowState::default()),
                (1, PresetShadowState::default()),
            ]),
            peq: HashMap::from([(
                1,
                minidsp::client::CachedPeqShadow {
                    coeffs: [1.0, 0.0, 0.0, 0.0, 0.0],
                    bypass: false,
                },
            )]),
            selector: HashMap::from([(2, 3)]),
            scalar: HashMap::from([(4, 5)]),
            preset_tokens: HashMap::from([
                (0, [0x11, 0x22, 0x33, 0x44]),
                (1, [0xaa, 0xbb, 0xcc, 0xdd]),
            ]),
            ..DeviceShadowState::default()
        };
        let filtered = filter_shadow_state_by_tokens(
            &state,
            &HashMap::from([(0, [0x11, 0x22, 0x33, 0x44]), (1, [1, 2, 3, 4])]),
        );
        assert!(filtered.peq.is_empty());
        assert!(filtered.selector.is_empty());
        assert!(filtered.scalar.is_empty());
        assert!(filtered.presets.contains_key(&0));
        assert!(!filtered.presets.contains_key(&1));
        assert_eq!(filtered.preset_tokens.get(&1), Some(&[1, 2, 3, 4]));
    }

    #[test]
    fn filter_shadow_state_preserves_legacy_values_when_tokens_match() {
        let state = DeviceShadowState {
            selector: HashMap::from([(2, 3)]),
            scalar: HashMap::from([(4, 5)]),
            preset_tokens: HashMap::from([(0, [1, 2, 3, 4])]),
            ..DeviceShadowState::default()
        };
        assert_eq!(
            filter_shadow_state_by_tokens(&state, &state.preset_tokens),
            state
        );
    }

    #[test]
    fn parse_official_setting_xml_extracts_timestamp_and_shadow_values() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../minidsp/tests/fixtures/official-setting-2x4hd-factory-reset.xml");
        let (selector_addrs, scalar_addrs) = official_xml_shadow_addrs(&probe::identify(10, 100));
        let (timestamp, preset) =
            parse_official_setting_xml(&path, &selector_addrs, &scalar_addrs, 100)
                .expect("parse xml");

        assert_eq!(timestamp, 0xfacc_93dd);
        assert_eq!(preset.selector.get(&6), Some(&2));
        assert_eq!(preset.selector.get(&14), Some(&3));
        assert_eq!(preset.selector.get(&84), Some(&6));
        assert_eq!(preset.scalar.get(&40), Some(&(-30.0f32).to_bits()));
        assert_eq!(preset.scalar.get(&42), Some(&(4.0f32).to_bits()));
        assert_eq!(preset.scalar.get(&44), Some(&(40.0f32).to_bits()));

        // <filter> blocks are parsed into peq shadow. The fixture has 68
        // identity-PEQ filters starting at addr 8280 (PEQ_1_1) stepping by 5.
        // Every band should land in the shadow keyed by its base address.
        assert!(
            preset.peq.len() >= 60,
            "expected ≥60 PEQ bands recovered from <filter> blocks, got {}",
            preset.peq.len()
        );
        let identity = preset.peq.get(&8280).expect("PEQ_1_1 at 8280");
        assert_eq!(identity.coeffs, [1.0, 0.0, 0.0, 0.0, 0.0]);
        assert!(!identity.bypass);
        assert_eq!(
            preset.peq.get(&8325).map(|p| p.coeffs),
            Some([1.0, 0.0, 0.0, 0.0, 0.0]),
            "PEQ_1_10 at 8325 should also be identity"
        );
    }

    #[test]
    fn parse_filter_hex_coeffs_decodes_five_floats() {
        // identity biquad: b0 = 1.0, others zero
        let got =
            parse_filter_hex_coeffs("3f800000,00000000,00000000,00000000,00000000").expect("parse");
        assert_eq!(got, [1.0, 0.0, 0.0, 0.0, 0.0]);

        // mixed signs and values
        let got =
            parse_filter_hex_coeffs("3f800000,bf800000,40000000,c0000000,3f000000").expect("parse");
        assert_eq!(got, [1.0, -1.0, 2.0, -2.0, 0.5]);
    }

    #[test]
    fn parse_filter_hex_coeffs_rejects_short_and_malformed() {
        // only 4 words — should reject
        assert!(parse_filter_hex_coeffs("3f800000,0,0,0").is_none());
        // non-hex garbage
        assert!(parse_filter_hex_coeffs("3f800000,not_hex,0,0,0").is_none());
    }

    #[test]
    fn parse_official_setting_xml_rejects_mismatched_dsp_version() {
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../minidsp/tests/fixtures/official-setting-2x4hd-factory-reset.xml");
        let temp = std::env::temp_dir().join(format!(
            "minidsp-open-mismatched-setting-{}-{}.xml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("time")
                .as_nanos()
        ));
        let content = fs::read_to_string(&source).expect("read source xml");
        let mismatched = content.replace(
            "<dspversion>100</dspversion>",
            "<dspversion>101</dspversion>",
        );
        fs::write(&temp, mismatched).expect("write temp xml");

        let (selector_addrs, scalar_addrs) = official_xml_shadow_addrs(&probe::identify(10, 100));
        let parsed = parse_official_setting_xml(&temp, &selector_addrs, &scalar_addrs, 100);

        let _ = fs::remove_file(&temp);
        assert!(parsed.is_none(), "mismatched dspversion should be rejected");
    }
}
