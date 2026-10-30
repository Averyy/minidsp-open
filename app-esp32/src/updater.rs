//! Auto-updater: downloads ezBEQ data from GitHub Pages to LittleFS.
//!
//! On boot (after WiFi connects), checks manifest.json for new versions.
//! Downloads catalogue.bin (biquad index) and UI files in 4KB streaming chunks.
//! Tracks installed version in NVS to skip redundant downloads.
//! Owns the background scheduling policy for periodic update checks.

#![allow(dead_code)] // Public API — used by main.rs and future modules

#[path = "bundle_update.rs"]
mod bundle;

use crate::storage;
use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::sys;
use log::{error, info, warn};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

/// Base URL for catalogue data on GitHub Pages.
const BASE_URL: &str = "https://averyy.github.io/minidsp-open";
const INITIAL_CHECK_DELAY: Duration = Duration::from_secs(30);
const PERIODIC_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// NVS namespace and key for tracking installed catalogue version.
const NVS_NAMESPACE: &str = "beq";
const NVS_VERSION_KEY: &str = "cat_ver";
#[cfg(feature = "dev-updater-override")]
const NVS_BASE_URL_KEY: &str = "base_url";

/// Chunk size for streaming downloads to flash (4KB).
const DOWNLOAD_CHUNK: usize = 4096;
const UPDATER_THREAD_STACK_BYTES: usize = 12 * 1024;
static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Cooldown between user-triggered refreshes so repeated 404s on a missing
/// GitHub Pages manifest don't burn TLS handshakes on every page load.
const TRIGGER_COOLDOWN_SECS: u32 = 5 * 60;
/// Monotonic seconds since boot of the most recent refresh attempt (0 = never).
static LAST_TRIGGER_SECS: AtomicU32 = AtomicU32::new(0);

fn uptime_secs() -> u32 {
    // esp_timer_get_time returns i64 microseconds since boot; cap at u32 seconds.
    let us = unsafe { sys::esp_timer_get_time() } as u64;
    (us / 1_000_000).min(u32::MAX as u64) as u32
}

/// Result of an update check.
#[derive(Debug)]
pub enum UpdateResult {
    /// Already up to date.
    Current,
    /// Downloaded new version.
    Updated { version: String, files: usize },
    /// No data partition available.
    NoStorage,
    /// Network or parse error (non-fatal).
    Error(String),
}

#[derive(Debug)]
pub enum UpdateAttempt {
    Completed(UpdateResult),
    Busy,
}

struct UpdateRunGuard;

impl UpdateRunGuard {
    fn acquire() -> Option<Self> {
        UPDATE_RUNNING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for UpdateRunGuard {
    fn drop(&mut self) {
        UPDATE_RUNNING.store(false, Ordering::Release);
    }
}

/// Manifest from the catalogue server.
#[derive(serde::Deserialize)]
struct Manifest {
    #[serde(default)]
    version: Option<String>,
    catalogue_version: String,
    #[serde(default)]
    ui_version: Option<String>,
    files: std::collections::HashMap<String, FileInfo>,
}

impl Manifest {
    /// Combined version string for NVS tracking (detects changes in either component).
    fn combined_version(&self) -> String {
        if let Some(version) = &self.version {
            return version.clone();
        }

        match &self.ui_version {
            Some(ui) => format!("{}+ui{}", self.catalogue_version, ui),
            None => self.catalogue_version.clone(),
        }
    }
}

#[derive(serde::Deserialize)]
struct FileInfo {
    size: u64,
    sha256: String,
}

/// Check for and download catalogue updates.
///
/// Call this after WiFi connects. Safe to call repeatedly — skips if already current.
pub fn check_and_update(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> UpdateResult {
    // Verify storage is mounted
    if storage::info().is_err() {
        return UpdateResult::NoStorage;
    }

    let base_url = base_url(nvs);
    let storage_installed = read_storage_version();
    let nvs_installed = read_nvs_version(nvs);
    if let Some(version) = storage_installed.as_deref() {
        if nvs_installed.as_deref() != Some(version) {
            if let Err(e) = write_nvs_version(nvs, version) {
                warn!("failed to sync installed version from storage to NVS: {e}");
            }
        }
    }
    let installed = storage_installed;
    let has_catalogue = has_catalogue_data();

    let remote_version = match fetch_remote_version(&base_url) {
        Ok(version) => version,
        Err(err) => {
            if should_keep_installed_data(installed.as_deref(), has_catalogue) {
                info!(
                    "catalogue update unavailable at {base_url}: {err}; keeping installed {}",
                    installed.as_deref().unwrap_or("(unknown)")
                );
                return UpdateResult::Current;
            }
            return UpdateResult::Error(format!("version fetch: {err}"));
        }
    };

    if !remote_version_requires_update(installed.as_deref(), &remote_version) {
        if installed.as_deref() == Some(remote_version.as_str()) {
            info!("catalogue up to date ({remote_version})");
        } else {
            info!(
                "keeping installed {} over remote {}",
                installed.as_deref().unwrap_or("(none)"),
                remote_version
            );
        }
        return UpdateResult::Current;
    }

    let manifest = match fetch_manifest(&base_url) {
        Ok(m) => m,
        Err(e) => return UpdateResult::Error(format!("manifest fetch: {e}")),
    };

    // Check installed version (combined catalogue + UI)
    let new_version = manifest.combined_version();
    if !remote_version_requires_update(installed.as_deref(), &new_version) {
        if installed.as_deref() == Some(new_version.as_str()) {
            info!("catalogue up to date ({new_version})");
        } else {
            info!(
                "keeping installed {} over manifest {}",
                installed.as_deref().unwrap_or("(none)"),
                new_version
            );
        }
        return UpdateResult::Current;
    }

    info!(
        "update available: {} -> {new_version}",
        installed.as_deref().unwrap_or("(none)"),
    );
    let _led = crate::led::begin_background_update();

    // Two current bundles exceed the storage partition, so files are replaced
    // one at a time. The durable journal prevents a partial bundle from ever
    // appearing current.
    if !manifest.files.contains_key("catalogue.bin") || new_version.trim().is_empty() {
        return UpdateResult::Error("manifest has no catalogue or version".into());
    }
    for filename in manifest.files.keys() {
        if !bundle::safe_filename(filename) {
            return UpdateResult::Error(format!("unsafe filename: {filename}"));
        }
    }
    // Discard downloads interrupted by reset before calculating free space.
    let root = storage::full_path("");
    if let Err(e) = bundle::discard_downloads(std::path::Path::new(&root)) {
        return UpdateResult::Error(format!("remove interrupted downloads: {e}"));
    }
    // Files already on flash with the manifest's size and hash need no
    // download. Besides saving bandwidth, this lets an update interrupted
    // after most renames resume: those files no longer count against free
    // space, which is otherwise too tight to replace them a second time.
    // version.txt is the commit marker, never an ordinary manifest asset.
    let pending: Vec<(&String, &FileInfo)> = manifest
        .files
        .iter()
        .filter(|(filename, info)| {
            filename.as_str() != "version.txt"
                && !installed_file_matches(&storage::full_path(filename), info)
        })
        .collect();
    let replacements: Vec<_> = pending
        .iter()
        .map(|(filename, info)| bundle::PendingFile {
            name: filename.as_str(),
            old_size: std::fs::metadata(storage::full_path(filename))
                .map(|metadata| metadata.len())
                .unwrap_or(0),
            new_size: info.size,
        })
        .collect();
    let available = match storage::info() {
        Ok(info) => info.total_bytes.saturating_sub(info.used_bytes) as u64,
        Err(e) => return UpdateResult::Error(format!("storage info: {e}")),
    };
    const SPACE_MARGIN: u64 = 64 * 1024;
    let plan = match bundle::replacement_plan(&replacements, available, SPACE_MARGIN) {
        Ok(plan) => plan,
        Err(e) => return UpdateResult::Error(e),
    };
    let delete_before_download = plan.delete_before_download;
    if delete_before_download {
        warn!(
            "not enough space to keep old files during the update ({} bytes free); replacing in place",
            available
        );
    }
    if let Err(e) = bundle::begin(std::path::Path::new(&root)) {
        return UpdateResult::Error(format!("begin bundle update: {e}"));
    }

    // Retain obsolete assets until commit so the currently published UI can
    // still load them. The planner accounts only for pending replacements.
    let mut downloaded = 0;
    for index in plan.order {
        let (filename, info) = pending[index];
        if delete_before_download {
            let _ = std::fs::remove_file(storage::full_path(filename));
        }
        match download_file(&base_url, filename, info) {
            Ok(()) => {
                info!("  downloaded {filename} ({} bytes)", info.size);
                downloaded += 1;
            }
            Err(e) => {
                error!("  failed to download {filename}: {e}");
                return UpdateResult::Error(format!("download {filename}: {e}"));
            }
        }
    }

    // Publish the marker only after every asset passed size/hash verification.
    if let Err(e) = bundle::commit(std::path::Path::new(&root), &new_version) {
        return UpdateResult::Error(format!("commit bundle version: {e}"));
    }
    // Content-hashed UI assets change name every release; without this the
    // partition fills with old releases until no update fits. An interrupted
    // reclaim only leaves dead files for the next update to remove.
    let keep: std::collections::HashSet<&str> = manifest.files.keys().map(String::as_str).collect();
    match bundle::reclaim_unreferenced(std::path::Path::new(&root), &keep) {
        Ok(0) => {}
        Ok(removed) => info!("removed {removed} file(s) no longer in the bundle"),
        Err(e) => warn!("failed to remove stale bundle files: {e}"),
    }
    if let Err(e) = write_nvs_version(nvs, &new_version) {
        warn!("failed to save version to NVS: {e}");
    }

    UpdateResult::Updated {
        version: new_version,
        files: downloaded,
    }
}

/// Try to run an update check, skipping if another update is already in progress.
pub fn try_check_and_update(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> UpdateAttempt {
    let _guard = match UpdateRunGuard::acquire() {
        Some(guard) => guard,
        None => {
            return UpdateAttempt::Busy;
        }
    };

    let result = check_and_update(nvs);
    UpdateAttempt::Completed(result)
}

/// Return whether an update check is currently running.
pub fn is_updating() -> bool {
    UPDATE_RUNNING.load(Ordering::Acquire)
}

/// Kick off a one-shot background refresh if no update is currently running.
///
/// Returns true if a new refresh task was started, false if an update was already running
/// or the thread could not be spawned.
pub fn trigger_background_refresh(
    nvs: esp_idf_svc::nvs::EspDefaultNvsPartition,
    reason: &'static str,
) -> bool {
    // Throttle user-triggered refreshes. Each attempt costs a TLS handshake
    // and — when the manifest 404s — only learns that it still 404s.
    let now = uptime_secs();
    let last = LAST_TRIGGER_SECS.load(Ordering::Acquire);
    if last != 0 && now.saturating_sub(last) < TRIGGER_COOLDOWN_SECS {
        return false;
    }

    let guard = match UpdateRunGuard::acquire() {
        Some(guard) => guard,
        None => return false,
    };

    match std::thread::Builder::new()
        .name("beq_refresh".into())
        .stack_size(UPDATER_THREAD_STACK_BYTES)
        .spawn(move || {
            info!("starting background catalogue refresh: {reason}");
            let _guard = guard;
            log_update_result(check_and_update(&nvs));
        }) {
        Ok(_) => {
            // Only mark the cooldown consumed once the refresh thread actually
            // started — otherwise spawn failures would block the next attempt
            // for 5 minutes even though no refresh ran.
            LAST_TRIGGER_SECS.store(now, Ordering::Release);
            true
        }
        Err(e) => {
            warn!("failed to spawn background catalogue refresh: {e}");
            false
        }
    }
}

/// Start the background updater thread.
pub fn start_background_updater(
    nvs: esp_idf_svc::nvs::EspDefaultNvsPartition,
) -> Result<(), std::io::Error> {
    std::thread::Builder::new()
        .name("beq_update".into())
        .stack_size(UPDATER_THREAD_STACK_BYTES)
        .spawn(move || {
            // Wait for WiFi to stabilize before hitting the network.
            std::thread::sleep(INITIAL_CHECK_DELAY);

            loop {
                run_scheduled_check(&nvs);
                std::thread::sleep(PERIODIC_CHECK_INTERVAL);
            }
        })
        .map(|_| ())
}

fn run_scheduled_check(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) {
    match try_check_and_update(nvs) {
        UpdateAttempt::Busy => {
            info!("scheduled catalogue check skipped: another update is already running");
        }
        UpdateAttempt::Completed(result) => log_update_result(result),
    }
}

fn should_keep_installed_data(installed: Option<&str>, has_catalogue_data: bool) -> bool {
    installed.is_some() && has_catalogue_data
}

fn log_update_result(result: UpdateResult) {
    match result {
        UpdateResult::Current => {}
        UpdateResult::Updated { version, files } => {
            info!("catalogue updated to {version} ({files} files)");
        }
        UpdateResult::NoStorage => {
            warn!("catalogue update skipped: no data partition");
        }
        UpdateResult::Error(e) => {
            warn!("catalogue update failed: {e}");
        }
    }
}

/// Fetch and parse the manifest.json.
fn fetch_manifest(base_url: &str) -> Result<Manifest, Box<dyn std::error::Error>> {
    let url = format!("{base_url}/manifest.json");
    let body = fetch_to_memory(&url, 8192)?;
    let manifest: Manifest = serde_json::from_slice(&body)?;
    Ok(manifest)
}

fn fetch_remote_version(base_url: &str) -> Result<String, Box<dyn std::error::Error>> {
    let url = format!("{base_url}/version.txt");
    let body = fetch_to_memory(&url, 256)?;
    let version = String::from_utf8(body)?;
    let trimmed = version.trim();
    if trimmed.is_empty() {
        return Err("empty version.txt".into());
    }
    Ok(trimmed.to_string())
}

/// Download a file from the catalogue server directly to LittleFS in chunks.
/// Verifies SHA-256 integrity after download; deletes corrupted files.
fn download_file(
    base_url: &str,
    filename: &str,
    info: &FileInfo,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{base_url}/{filename}");
    let path = storage::full_path(filename);
    let temp_path = format!("{path}.part");

    // Create parent directories
    if let Some(parent) = std::path::Path::new(&path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let result: Result<(), Box<dyn std::error::Error>> = (|| {
        let mut conn = create_connection(Duration::from_secs(120))?;
        conn.initiate_request(Method::Get, &url, &[])?;
        conn.initiate_response()?;
        let status = conn.status();
        if status != 200 {
            return Err(format!("HTTP {status} for {url}").into());
        }

        // Stream to a temporary file in chunks, computing SHA-256 on the fly.
        let mut file = std::fs::File::create(&temp_path)?;
        let mut buf = [0u8; DOWNLOAD_CHUNK];
        let mut total = 0u64;
        let mut hasher = Sha256::new();

        loop {
            let n = conn.read(&mut buf)?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > info.size {
                return Err(format!("download exceeds manifest size for {filename}").into());
            }
            std::io::Write::write_all(&mut file, &buf[..n])?;
            hasher.update(&buf[..n]);
        }

        if total != info.size {
            return Err(format!(
                "size mismatch for {filename}: got {total}, expected {}",
                info.size
            )
            .into());
        }

        let digest = hasher.finish_hex();
        if digest != info.sha256 {
            return Err(format!(
                "SHA-256 mismatch for {filename}: got {digest}, expected {}",
                info.sha256
            )
            .into());
        }

        file.sync_all()?;
        Ok(())
    })();

    if let Err(err) = result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(err);
    }

    std::fs::rename(&temp_path, &path)?;
    Ok(())
}

/// True if `path` already holds exactly the file `info` describes.
fn installed_file_matches(path: &str, info: &FileInfo) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != info.size {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut hasher = Sha256::new();
    let mut buf = [0u8; DOWNLOAD_CHUNK];
    loop {
        match std::io::Read::read(&mut file, &mut buf) {
            Ok(0) => break,
            Ok(n) => hasher.update(&buf[..n]),
            Err(_) => return false,
        }
    }
    hasher.finish_hex() == info.sha256
}

/// Fetch a small URL entirely into memory.
fn fetch_to_memory(url: &str, max_size: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut conn = create_connection(Duration::from_secs(10))?;
    conn.initiate_request(Method::Get, url, &[])?;
    conn.initiate_response()?;
    let status = conn.status();
    if status != 200 {
        return Err(format!("HTTP {status} for {url}").into());
    }

    let mut body = Vec::with_capacity(max_size.min(4096));
    let mut buf = [0u8; 1024];
    loop {
        let n = conn.read(&mut buf)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
        if body.len() > max_size {
            return Err("response too large".into());
        }
    }

    Ok(body)
}

/// Create an HTTP connection with TLS cert bundle for HTTPS.
fn create_connection(timeout: Duration) -> Result<EspHttpConnection, Box<dyn std::error::Error>> {
    let config = Configuration {
        buffer_size: Some(DOWNLOAD_CHUNK),
        timeout: Some(timeout),
        crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
        ..Default::default()
    };
    let conn = EspHttpConnection::new(&config)?;
    Ok(conn)
}

/// Read the installed catalogue version from NVS.
fn read_nvs_version(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> Option<String> {
    use esp_idf_svc::nvs::EspNvs;

    let nvs = EspNvs::new(nvs.clone(), NVS_NAMESPACE, false).ok()?;
    let mut buf = [0u8; 128];
    let val = nvs.get_str(NVS_VERSION_KEY, &mut buf).ok()?;
    val.map(|s| s.to_string())
}

fn read_storage_version() -> Option<String> {
    bundle::installed_version(std::path::Path::new(&storage::full_path("")))
}

fn version_sort_key(version: &str) -> Option<&str> {
    let bytes = version.as_bytes();
    if bytes.len() < 16 {
        return None;
    }
    if bytes[8] != b'T' || bytes[15] != b'Z' {
        return None;
    }
    let digit_positions = [0usize, 1, 2, 3, 4, 5, 6, 7, 9, 10, 11, 12, 13, 14];
    if digit_positions
        .iter()
        .any(|&idx| !bytes[idx].is_ascii_digit())
    {
        return None;
    }
    Some(&version[..16])
}

fn remote_version_requires_update(installed: Option<&str>, remote: &str) -> bool {
    let Some(installed) = installed else {
        return true;
    };
    if installed == remote {
        return false;
    }

    match (version_sort_key(installed), version_sort_key(remote)) {
        (Some(installed_key), Some(remote_key)) => {
            if remote_key != installed_key {
                remote_key > installed_key
            } else {
                true
            }
        }
        (Some(_), None) => false,
        (None, Some(_)) => true,
        (None, None) => true,
    }
}

#[cfg(feature = "dev-updater-override")]
fn read_nvs_base_url(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> Option<String> {
    use esp_idf_svc::nvs::EspNvs;

    let nvs = EspNvs::new(nvs.clone(), NVS_NAMESPACE, false).ok()?;
    let mut buf = [0u8; 256];
    let val = nvs.get_str(NVS_BASE_URL_KEY, &mut buf).ok()?;
    val.map(|s| s.to_string())
}

/// Write the installed catalogue version to NVS.
fn write_nvs_version(
    nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition,
    version: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use esp_idf_svc::nvs::EspNvs;

    let nvs = EspNvs::new(nvs.clone(), NVS_NAMESPACE, true)?;
    nvs.set_str(NVS_VERSION_KEY, version)?;
    Ok(())
}

#[cfg(feature = "dev-updater-override")]
fn write_nvs_base_url(
    nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition,
    url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use esp_idf_svc::nvs::EspNvs;

    let nvs = EspNvs::new(nvs.clone(), NVS_NAMESPACE, true)?;
    nvs.set_str(NVS_BASE_URL_KEY, url)?;
    Ok(())
}

/// Get the currently installed catalogue version (if any).
pub fn installed_version(_nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> Option<String> {
    read_storage_version()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_version_tracks_ui_when_manifest_version_exists() {
        let manifest = Manifest {
            version: Some("2026.04.13".into()),
            catalogue_version: "2026.04.13".into(),
            ui_version: Some("42".into()),
            files: std::collections::HashMap::new(),
        };

        assert_eq!(manifest.combined_version(), "2026.04.13");
    }

    #[test]
    fn combined_version_falls_back_to_catalogue_when_ui_missing() {
        let manifest = Manifest {
            version: None,
            catalogue_version: "2026.04.13".into(),
            ui_version: None,
            files: std::collections::HashMap::new(),
        };

        assert_eq!(manifest.combined_version(), "2026.04.13");
    }

    #[test]
    fn keep_installed_data_requires_version_and_catalogue() {
        assert!(should_keep_installed_data(Some("2026.04.13"), true));
        assert!(!should_keep_installed_data(Some("2026.04.13"), false));
        assert!(!should_keep_installed_data(None, true));
    }

    #[test]
    fn version_sort_key_parses_timestamped_versions() {
        assert_eq!(
            version_sort_key("20260415T124249Z+cat1b47227beded+ui2.6.1+site123456789abc"),
            Some("20260415T124249Z")
        );
        assert_eq!(version_sort_key("1b47227beded4115"), None);
    }

    #[test]
    fn remote_version_requires_update_uses_timestamped_ordering() {
        assert!(remote_version_requires_update(
            Some("20260415T124249Z+catold+ui2.6.0+siteaaaa"),
            "20260415T124300Z+catnew+ui2.6.1+sitebbbb"
        ));
        assert!(!remote_version_requires_update(
            Some("20260415T124300Z+catnew+ui2.6.1+sitebbbb"),
            "20260415T124249Z+catold+ui2.6.0+siteaaaa"
        ));
    }

    #[test]
    fn remote_version_requires_update_rejects_legacy_downgrade() {
        assert!(!remote_version_requires_update(
            Some("20260415T124300Z+catnew+ui2.6.1+sitebbbb"),
            "1b47227beded4115d1b14fbe3185f795a82d95bb+ui2.6.1+siteaaaa"
        ));
        assert!(remote_version_requires_update(
            Some("1b47227beded4115d1b14fbe3185f795a82d95bb+ui2.6.1+siteaaaa"),
            "20260415T124300Z+catnew+ui2.6.1+sitebbbb"
        ));
    }

    #[test]
    fn remote_version_requires_update_updates_same_stamp_different_payload() {
        assert!(remote_version_requires_update(
            Some("20260415T124300Z+catnew+ui2.6.1+siteaaaa"),
            "20260415T124300Z+catnew+ui2.6.1+sitebbbb"
        ));
    }
}

/// Get the configured updater base URL, falling back to the built-in default.
pub fn base_url(_nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> String {
    #[cfg(feature = "dev-updater-override")]
    if let Some(url) = read_nvs_base_url(_nvs).filter(|s| !s.trim().is_empty()) {
        return url;
    }
    BASE_URL.to_string()
}

/// Persist an updater base URL override. Empty string clears the override.
#[cfg(feature = "dev-updater-override")]
pub fn set_base_url(
    nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition,
    url: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let normalized = url.trim().trim_end_matches('/').to_string();
    if normalized.is_empty() {
        write_nvs_base_url(nvs, "")?;
        return Ok(BASE_URL.to_string());
    }
    if !(normalized.starts_with("http://") || normalized.starts_with("https://")) {
        return Err("base_url must start with http:// or https://".into());
    }
    write_nvs_base_url(nvs, &normalized)?;
    Ok(normalized)
}

/// Check if catalogue data files exist on the data partition.
pub fn has_catalogue_data() -> bool {
    storage::file_exists("catalogue.bin")
}

/// Portable SHA-256 shared by bundle downloads and firmware OTA verification.
pub(crate) struct Sha256 {
    ctx: sha2::Sha256,
}

impl Sha256 {
    pub(crate) fn new() -> Self {
        use sha2::Digest;
        Self {
            ctx: sha2::Sha256::new(),
        }
    }

    pub(crate) fn update(&mut self, data: &[u8]) {
        use sha2::Digest;
        self.ctx.update(data);
    }

    pub(crate) fn finish_hex(self) -> String {
        use sha2::Digest;
        let hash = self.ctx.finalize();
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }
}
