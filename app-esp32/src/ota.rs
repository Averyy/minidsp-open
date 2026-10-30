//! Firmware over-the-air updates.
//!
//! Two paths write the inactive app slot: a raw image uploaded to
//! `POST /firmware` (local builds, `tools/ota-push.sh`) and the latest GitHub
//! release (`POST /firmware/update`; the board checks for one daily). ESP-IDF
//! validates the image (header, segments, appended SHA-256) in `esp_ota_end`
//! before the boot slot switches. With `CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE`
//! a new image boots as pending-verify, and the bootloader goes back to the
//! previous slot unless `start_health_check` confirms this firmware works.

use crate::updater::Sha256;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::ota::EspOta;
use esp_idf_svc::sys;
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const FIRMWARE_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const FIRMWARE_BUILD: &str = env!("MINIDSP_BUILD_REV");

const RELEASE_MANIFEST_URL: &str =
    "https://github.com/Averyy/minidsp-open/releases/latest/download/firmware.json";
const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/Averyy/minidsp-open/releases/download";
const MAX_MANIFEST_BYTES: usize = 4096;
const CHUNK_BYTES: usize = 4096;
/// `esp_image_header_t.chip_id` for the ESP32-S3.
const ESP32S3_CHIP_ID: u16 = 9;
const ESP_IMAGE_MAGIC: u8 = 0xE9;
/// Enough of the image to check its header before writing anything.
const IMAGE_HEADER_BYTES: usize = 24;

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(120);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// A healthy boot must last this long before the image is confirmed.
const HEALTHY_UPTIME: Duration = Duration::from_secs(30);
/// Give up on an unconfirmed image after this long and roll back.
const ROLLBACK_DEADLINE: Duration = Duration::from_secs(180);
const REBOOT_DELAY: Duration = Duration::from_millis(1500);

static OTA: OnceLock<Mutex<EspOta>> = OnceLock::new();
static UPDATE_BUSY: AtomicBool = AtomicBool::new(false);
/// Set once an HTTP server carrying the `/firmware` routes is listening.
static HTTP_READY: AtomicBool = AtomicBool::new(false);
static STATUS: Mutex<OtaProgressState> = Mutex::new(OtaProgressState {
    latest: None,
    progress: None,
});

struct OtaProgressState {
    latest: Option<ReleaseCheck>,
    progress: Option<Progress>,
}

/// `GET /firmware`
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirmwareStatus {
    pub version: &'static str,
    pub build: &'static str,
    pub running_slot: String,
    /// `valid`, `pendingVerify` (just updated, not yet confirmed), or `unknown`.
    pub slot_state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest: Option<ReleaseCheck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<Progress>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseCheck {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub update_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_notes_url: Option<String>,
    pub checked_at_uptime_sec: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `upload` or `release`.
    pub source: &'static str,
    /// `checking`, `writing`, `rebooting`, or `failed`.
    pub state: &'static str,
    pub bytes_written: usize,
    pub total_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `firmware.json`, published with every GitHub release.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReleaseManifest {
    version: String,
    chip: String,
    app: ReleaseAsset,
    #[serde(default)]
    release_notes_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    name: String,
    size: usize,
    sha256: String,
}

#[derive(Debug, Clone)]
pub enum OtaError {
    /// Another update is already writing the inactive slot.
    Busy,
    /// The image or request is unacceptable (wrong chip, size, hash, ...).
    Rejected(String),
    /// Network, flash, or driver failure.
    Failed(String),
}

impl std::fmt::Display for OtaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy => write!(f, "a firmware update is already in progress"),
            Self::Rejected(msg) | Self::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

fn ota() -> Result<std::sync::MutexGuard<'static, EspOta>, OtaError> {
    if OTA.get().is_none() {
        let ota = EspOta::new().map_err(|e| OtaError::Failed(format!("OTA unavailable: {e}")))?;
        let _ = OTA.set(Mutex::new(ota));
    }
    Ok(OTA
        .get()
        .expect("OTA singleton initialised above")
        .lock()
        .unwrap_or_else(|e| e.into_inner()))
}

/// Running slot label and state, read straight from ESP-IDF: the `EspOta`
/// singleton is held for the whole of a release download, and a status poll
/// must not wait on it.
fn running_slot() -> (String, &'static str) {
    // SAFETY: returns a pointer into the static partition table, or null.
    let Some(partition) = (unsafe { sys::esp_ota_get_running_partition().as_ref() }) else {
        return ("unknown".to_string(), "unknown");
    };
    let label = unsafe { std::ffi::CStr::from_ptr(partition.label.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let mut state: sys::esp_ota_img_states_t = 0;
    let err = unsafe { sys::esp_ota_get_state_partition(partition, &mut state) };
    let state = if err != sys::ESP_OK {
        // No otadata entry yet (first boot after a USB flash): nothing to verify.
        "valid"
    } else if state == sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY {
        "pendingVerify"
    } else if state == sys::esp_ota_img_states_t_ESP_OTA_IMG_VALID
        || state == sys::esp_ota_img_states_t_ESP_OTA_IMG_UNDEFINED
        || state == sys::esp_ota_img_states_t_ESP_OTA_IMG_NEW
    {
        "valid"
    } else {
        "unknown"
    };
    (label, state)
}

pub fn status() -> FirmwareStatus {
    let (running_slot, slot_state) = running_slot();
    let state = STATUS.lock().unwrap_or_else(|e| e.into_inner());
    FirmwareStatus {
        version: FIRMWARE_VERSION,
        build: FIRMWARE_BUILD,
        running_slot,
        slot_state,
        latest: state.latest.clone(),
        progress: state.progress.clone(),
    }
}

/// Called once the HTTP server (with the `/firmware` routes) is listening.
/// A new image is only confirmed after this, so a build whose server fails
/// to start still rolls back.
pub fn mark_http_ready() {
    HTTP_READY.store(true, Ordering::SeqCst);
}

/// Releases `UPDATE_BUSY` however a write attempt ends.
struct BusyGuard;

impl BusyGuard {
    fn claim() -> Result<Self, OtaError> {
        UPDATE_BUSY
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| Self)
            .map_err(|_| OtaError::Busy)
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        UPDATE_BUSY.store(false, Ordering::SeqCst);
    }
}

fn set_progress(progress: Option<Progress>) {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).progress = progress;
}

/// Size of the slot the next update will be written to.
fn update_slot_size() -> Result<usize, OtaError> {
    // SAFETY: returns a pointer into the static partition table, or null.
    let partition = unsafe { sys::esp_ota_get_next_update_partition(std::ptr::null()).as_ref() };
    partition
        .map(|partition| partition.size as usize)
        .ok_or_else(|| OtaError::Failed("no OTA slot available (old partition table?)".into()))
}

fn check_image_header(header: &[u8]) -> Result<(), OtaError> {
    if header.len() < IMAGE_HEADER_BYTES || header[0] != ESP_IMAGE_MAGIC {
        return Err(OtaError::Rejected(
            "not an ESP-IDF app image (expected the elf2image .bin, not the ELF or a merged image)"
                .into(),
        ));
    }
    let chip_id = u16::from_le_bytes([header[12], header[13]]);
    if chip_id != ESP32S3_CHIP_ID {
        return Err(OtaError::Rejected(format!(
            "image is for chip id {chip_id}, this board is an ESP32-S3 ({ESP32S3_CHIP_ID})"
        )));
    }
    Ok(())
}

/// Stream an app image of `total_bytes` into the inactive slot and make it
/// the boot slot. `read` fills the buffer and returns the byte count (0 at
/// the end). The running firmware is untouched unless everything verifies.
pub fn write_image(
    source: &'static str,
    total_bytes: usize,
    expected_sha256: Option<&str>,
    mut read: impl FnMut(&mut [u8]) -> Result<usize, String>,
) -> Result<(), OtaError> {
    let busy = BusyGuard::claim()?;
    let result = write_image_claimed(source, total_bytes, expected_sha256, &mut read);
    finish_attempt(busy, source, total_bytes, &result);
    result
}

/// Record how an update attempt ended. On success the boot slot has switched
/// and a reboot follows, so the busy flag stays held to refuse further writes.
fn finish_attempt(
    busy: BusyGuard,
    source: &'static str,
    total_bytes: usize,
    result: &Result<(), OtaError>,
) {
    match result {
        Ok(()) => std::mem::forget(busy),
        Err(err) => {
            warn!("firmware update from {source} failed: {err}");
            set_progress(Some(Progress {
                source,
                state: "failed",
                bytes_written: 0,
                total_bytes,
                error: Some(err.to_string()),
            }));
        }
    }
}

fn write_image_claimed(
    source: &'static str,
    total_bytes: usize,
    expected_sha256: Option<&str>,
    read: &mut impl FnMut(&mut [u8]) -> Result<usize, String>,
) -> Result<(), OtaError> {
    if total_bytes < IMAGE_HEADER_BYTES {
        return Err(OtaError::Rejected("image is empty or truncated".into()));
    }
    let slot_size = update_slot_size()?;
    if total_bytes > slot_size {
        return Err(OtaError::Rejected(format!(
            "image is {total_bytes} bytes; the OTA slot holds {slot_size}"
        )));
    }
    let expected_sha256 = expected_sha256
        .map(|sha| sha.trim().to_ascii_lowercase())
        .filter(|sha| !sha.is_empty());
    if let Some(sha) = &expected_sha256 {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(OtaError::Rejected(
                "sha256 must be 64 hex characters".into(),
            ));
        }
    }

    // Validate the header before touching flash.
    let mut buf = vec![0u8; CHUNK_BYTES];
    let mut buffered = 0;
    while buffered < IMAGE_HEADER_BYTES {
        let n = read(&mut buf[buffered..]).map_err(OtaError::Failed)?;
        if n == 0 {
            return Err(OtaError::Rejected("image is truncated".into()));
        }
        buffered += n;
    }
    check_image_header(&buf[..buffered])?;

    info!("firmware update from {source}: writing {total_bytes} bytes to the inactive slot");
    let _led = crate::led::begin_background_update();
    let mut ota = ota()?;
    let mut update = ota
        .initiate_update_with_known_size(total_bytes)
        .map_err(|e| OtaError::Failed(format!("start OTA write: {e}")))?;
    let mut hasher = Sha256::new();
    let mut written = 0usize;
    let mut last_reported = 0usize;
    loop {
        if buffered > 0 {
            if written + buffered > total_bytes {
                return Err(OtaError::Rejected(format!(
                    "image is longer than the declared {total_bytes} bytes"
                )));
            }
            update
                .write(&buf[..buffered])
                .map_err(|e| OtaError::Failed(format!("flash write: {e}")))?;
            hasher.update(&buf[..buffered]);
            written += buffered;
            if written - last_reported >= 64 * 1024 || written == total_bytes {
                last_reported = written;
                set_progress(Some(Progress {
                    source,
                    state: "writing",
                    bytes_written: written,
                    total_bytes,
                    error: None,
                }));
            }
        }
        if written == total_bytes {
            break;
        }
        buffered = read(&mut buf).map_err(OtaError::Failed)?;
        if buffered == 0 {
            return Err(OtaError::Rejected(format!(
                "image ended after {written} of {total_bytes} bytes"
            )));
        }
    }

    let digest = hasher.finish_hex();
    if let Some(expected) = expected_sha256 {
        if digest != expected {
            // Dropping `update` aborts it; the boot slot stays unchanged.
            return Err(OtaError::Rejected(format!(
                "sha256 mismatch: got {digest}, expected {expected}"
            )));
        }
    }
    // esp_ota_end validates the whole image before the boot slot switches.
    update
        .complete()
        .map_err(|e| OtaError::Rejected(format!("image failed ESP-IDF validation: {e}")))?;
    info!("firmware update from {source} verified (sha256 {digest}); boot slot switched");
    set_progress(Some(Progress {
        source,
        state: "rebooting",
        bytes_written: written,
        total_bytes,
        error: None,
    }));
    Ok(())
}

/// Restart shortly, so the HTTP response that triggered it can go out.
pub fn schedule_reboot() {
    let spawned = std::thread::Builder::new()
        .name("ota_reboot".into())
        .stack_size(4096)
        .spawn(|| {
            std::thread::sleep(REBOOT_DELAY);
            info!("rebooting into the new firmware");
            unsafe { sys::esp_restart() };
        });
    if let Err(err) = spawned {
        error!("failed to schedule reboot ({err}); rebooting now");
        unsafe { sys::esp_restart() };
    }
}

fn uptime_sec() -> u64 {
    (unsafe { sys::esp_timer_get_time() } / 1_000_000) as u64
}

fn http_get(url: &str, timeout: Duration) -> Result<EspHttpConnection, String> {
    let config = Configuration {
        // GitHub release downloads redirect to a CDN URL longer than the
        // default 512-byte header buffer.
        buffer_size: Some(CHUNK_BYTES),
        timeout: Some(timeout),
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let mut conn = EspHttpConnection::new(&config).map_err(|e| format!("HTTP client: {e}"))?;
    conn.initiate_request(Method::Get, url, &[("User-Agent", "minidsp-open")])
        .map_err(|e| format!("GET {url}: {e}"))?;
    conn.initiate_response()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let status = conn.status();
    if status != 200 {
        return Err(format!("GET {url}: HTTP {status}"));
    }
    Ok(conn)
}

fn fetch_release_manifest() -> Result<ReleaseManifest, String> {
    let mut conn = http_get(RELEASE_MANIFEST_URL, Duration::from_secs(20))?;
    let mut body = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = conn
            .read(&mut buf)
            .map_err(|e| format!("read firmware.json: {e}"))?;
        if n == 0 {
            break;
        }
        if body.len() + n > MAX_MANIFEST_BYTES {
            return Err("firmware.json is too large".into());
        }
        body.extend_from_slice(&buf[..n]);
    }
    serde_json::from_slice(&body).map_err(|e| format!("parse firmware.json: {e}"))
}

/// Ask GitHub for the latest release and record the answer for `status()`.
fn check_for_update() -> ReleaseCheck {
    let checked_at_uptime_sec = uptime_sec();
    let check = match fetch_release_manifest() {
        Ok(manifest) => {
            let newer = is_newer_version(&manifest.version, FIRMWARE_VERSION);
            if newer {
                info!(
                    "firmware {} is available (running {FIRMWARE_VERSION})",
                    manifest.version
                );
            }
            ReleaseCheck {
                update_available: newer && manifest.chip == "esp32s3",
                version: Some(manifest.version),
                release_notes_url: manifest.release_notes_url,
                checked_at_uptime_sec,
                error: None,
            }
        }
        Err(err) => {
            warn!("firmware update check failed: {err}");
            ReleaseCheck {
                version: None,
                update_available: false,
                release_notes_url: None,
                checked_at_uptime_sec,
                error: Some(err),
            }
        }
    };
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).latest = Some(check.clone());
    check
}

/// Install the latest GitHub release in the background and reboot into it.
/// The busy flag is claimed before returning, so repeated requests can't
/// start parallel downloads; progress and failures show in `status()`.
pub fn spawn_release_install(stack_size: usize) -> Result<(), OtaError> {
    let busy = BusyGuard::claim()?;
    set_progress(Some(Progress {
        source: "release",
        state: "checking",
        bytes_written: 0,
        total_bytes: 0,
        error: None,
    }));
    std::thread::Builder::new()
        .name("fw_install".into())
        .stack_size(stack_size)
        .spawn(move || {
            let result = install_latest_release_claimed();
            let total_bytes = STATUS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .progress
                .as_ref()
                .map_or(0, |progress| progress.total_bytes);
            let outcome = result.as_ref().map(|_| ()).map_err(OtaError::clone);
            finish_attempt(busy, "release", total_bytes, &outcome);
            if let Ok(version) = result {
                info!("installed firmware {version} from GitHub");
                schedule_reboot();
            }
        })
        .map(|_| ())
        .map_err(|e| {
            // The closure (and the guard it owns) was dropped with the error.
            set_progress(None);
            OtaError::Failed(format!("spawn install: {e}"))
        })
}

/// Download the latest release's app image into the inactive slot. Returns
/// the installed version. The caller holds the busy flag.
fn install_latest_release_claimed() -> Result<String, OtaError> {
    let manifest = fetch_release_manifest().map_err(OtaError::Failed)?;
    if manifest.chip != "esp32s3" {
        return Err(OtaError::Rejected(format!(
            "release is for {}, not esp32s3",
            manifest.chip
        )));
    }
    if !is_newer_version(&manifest.version, FIRMWARE_VERSION) {
        return Err(OtaError::Rejected(format!(
            "release {} is not newer than the running {FIRMWARE_VERSION}",
            manifest.version
        )));
    }
    if manifest.app.name.contains(['/', '?', '#', '\\']) {
        return Err(OtaError::Rejected(
            "release asset name is not a plain file name".into(),
        ));
    }
    let url = format!(
        "{RELEASE_DOWNLOAD_BASE}/v{}/{}",
        manifest.version, manifest.app.name
    );
    let mut conn = http_get(&url, Duration::from_secs(60)).map_err(OtaError::Failed)?;
    write_image_claimed(
        "release",
        manifest.app.size,
        Some(&manifest.app.sha256),
        &mut |buf| conn.read(buf).map_err(|e| format!("download {url}: {e}")),
    )?;
    Ok(manifest.version)
}

const CHECK_COOLDOWN: Duration = Duration::from_secs(30);
static CHECK_WORKER: crate::threads::Worker =
    crate::threads::Worker::new(c"fw_check", "fw_check", 12 * 1024);
static CHECK_WAKE: Condvar = Condvar::new();
static CHECK_STATE: Mutex<CheckState> = Mutex::new(CheckState {
    started: false,
    pending: false,
    last_finished: None,
});

struct CheckState {
    started: bool,
    pending: bool,
    last_finished: Option<Instant>,
}

/// Queue at most one manual check on the permanent checker. The cooldown also
/// covers failed network attempts, so bursts cannot exhaust tasks or sockets.
pub fn request_check() -> Result<(), &'static str> {
    start_background_checker().map_err(|_| "firmware checker unavailable")?;
    let mut state = CHECK_STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.pending {
        return Err("check in progress");
    }
    if state
        .last_finished
        .is_some_and(|t| t.elapsed() < CHECK_COOLDOWN)
    {
        return Err("check cooldown; retry after 30 seconds");
    }
    state.pending = true;
    CHECK_WAKE.notify_one();
    Ok(())
}

/// Daily release check and manual triggers share one persistent PSRAM worker.
pub fn start_background_checker() -> Result<(), String> {
    let mut state = CHECK_STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.started {
        return Ok(());
    }
    CHECK_WORKER.run(|| {
        let mut next_check = Instant::now() + FIRST_CHECK_DELAY;
        loop {
            let mut state = CHECK_STATE.lock().unwrap_or_else(|e| e.into_inner());
            while !state.pending && Instant::now() < next_check {
                let wait = next_check.saturating_duration_since(Instant::now());
                state = CHECK_WAKE
                    .wait_timeout(state, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            state.pending = true;
            drop(state);
            check_for_update();
            let mut state = CHECK_STATE.lock().unwrap_or_else(|e| e.into_inner());
            state.pending = false;
            state.last_finished = Some(Instant::now());
            next_check = Instant::now() + CHECK_INTERVAL;
        }
    })?;
    state.started = true;
    Ok(())
}

/// Confirm a freshly updated image once it has proven it can be reached
/// again, or roll back. Reachable means STA WiFi is up (so the next fix can
/// arrive over the network), or the board has no WiFi credentials and is
/// legitimately in SoftAP provisioning mode.
pub fn start_health_check(has_wifi_credentials: bool) -> Result<(), std::io::Error> {
    std::thread::Builder::new()
        .name("ota_health".into())
        .stack_size(6 * 1024)
        .spawn(move || {
            if running_slot().1 != "pendingVerify" {
                return;
            }
            info!("firmware {FIRMWARE_VERSION} is pending verification");
            let booted = Instant::now();
            loop {
                std::thread::sleep(Duration::from_secs(5));
                // Reachable means a client can get to /firmware again: the
                // HTTP server is up, on STA WiFi or (with no credentials
                // saved) on the provisioning SoftAP.
                let network_up = crate::wifi::is_connected() || !has_wifi_credentials;
                let reachable = network_up && HTTP_READY.load(Ordering::SeqCst);
                if reachable && booted.elapsed() >= HEALTHY_UPTIME {
                    match ota().map(|mut ota| ota.mark_running_slot_valid()) {
                        Ok(Ok(())) => info!("firmware {FIRMWARE_VERSION} confirmed"),
                        Ok(Err(e)) => error!("failed to confirm firmware: {e}"),
                        Err(e) => error!("failed to confirm firmware: {e}"),
                    }
                    return;
                }
                if booted.elapsed() >= ROLLBACK_DEADLINE {
                    error!(
                        "firmware {FIRMWARE_VERSION} never became reachable over HTTP; rolling back"
                    );
                    if let Ok(mut ota) = ota() {
                        let err = ota.mark_running_slot_invalid_and_reboot();
                        error!("rollback failed: {err}");
                    }
                    return;
                }
            }
        })
        .map(|_| ())
}

/// True if `candidate` is a strictly newer semver than `current`. A
/// pre-release sorts before its release; build metadata is ignored.
pub fn is_newer_version(candidate: &str, current: &str) -> bool {
    match (parse_version(candidate), parse_version(current)) {
        (Some(candidate), Some(current)) => candidate > current,
        _ => false,
    }
}

/// (major, minor, patch, is_release, pre-release text) — orders like semver
/// except that pre-release identifiers compare as plain strings.
fn parse_version(version: &str) -> Option<(u64, u64, u64, bool, String)> {
    let version = version.trim().trim_start_matches('v');
    let version = version.split('+').next()?;
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut parts = core.split('.').map(|part| part.parse::<u64>().ok());
    let version = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    Some((
        version.0,
        version.1,
        version.2,
        pre.is_none(),
        pre.unwrap_or_default().to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{check_image_header, is_newer_version};

    #[test]
    fn version_ordering_follows_semver() {
        assert!(is_newer_version("0.2.1", "0.2.0"));
        assert!(is_newer_version("v1.0.0", "0.9.9"));
        assert!(is_newer_version("0.3.0", "0.3.0-rc.1"));
        assert!(!is_newer_version("0.3.0-rc.1", "0.3.0"));
        assert!(!is_newer_version("0.2.0", "0.2.0"));
        assert!(!is_newer_version("garbage", "0.2.0"));
    }

    #[test]
    fn image_header_must_be_an_esp32s3_app() {
        let mut header = [0u8; 24];
        header[0] = 0xE9;
        header[12] = 9;
        assert!(check_image_header(&header).is_ok());
        header[12] = 0; // ESP32
        assert!(check_image_header(&header).is_err());
        header[0] = 0x7F; // ELF magic
        assert!(check_image_header(&header).is_err());
    }
}
