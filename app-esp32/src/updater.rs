//! Auto-updater: downloads ezBEQ data from GitHub Pages to LittleFS.
//!
//! On boot (after WiFi connects), checks manifest.json for new versions.
//! Downloads catalogue.bin (biquad index) and UI files in 4KB streaming chunks.
//! Tracks installed version in NVS to skip redundant downloads.
//! Owns the background scheduling policy for periodic update checks.

#![allow(dead_code)] // Public API — used by main.rs and future modules

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
const NVS_BASE_URL_KEY: &str = "base_url";

/// Chunk size for streaming downloads to flash (4KB).
const DOWNLOAD_CHUNK: usize = 4096;
const UPDATER_THREAD_STACK_BYTES: usize = 16 * 1024;
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
        match (&self.version, &self.ui_version) {
            (Some(version), Some(ui)) => format!("{version}+ui{ui}"),
            (Some(version), None) => version.clone(),
            (None, Some(ui)) => format!("{}+ui{}", self.catalogue_version, ui),
            (None, None) => self.catalogue_version.clone(),
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

    // Fetch manifest
    let base_url = base_url(nvs);
    let manifest = match fetch_manifest(&base_url) {
        Ok(m) => m,
        Err(e) => return UpdateResult::Error(format!("manifest fetch: {e}")),
    };

    // Check installed version (combined catalogue + UI)
    let new_version = manifest.combined_version();
    let installed = read_nvs_version(nvs);
    if installed.as_deref() == Some(new_version.as_str()) {
        info!("catalogue up to date ({new_version})");
        return UpdateResult::Current;
    }

    info!(
        "update available: {} -> {new_version}",
        installed.as_deref().unwrap_or("(none)"),
    );
    let _led = crate::led::begin_background_update();

    // Clear installed version FIRST — if anything fails, next boot retries everything.
    // Bail out on failure: if we can't clear NVS and then delete the on-flash
    // files, a later download failure would leave the old version string pointing
    // at an empty data partition and the next boot would believe it was current.
    if let Err(e) = write_nvs_version(nvs, "") {
        error!("failed to clear NVS version before update: {e}");
        return UpdateResult::Error(format!("clear NVS version: {e}"));
    }

    // Delete all existing data files before downloading new ones (all-or-nothing)
    delete_data_files();

    // Download each file
    let mut downloaded = 0;
    for (filename, info) in &manifest.files {
        // Sanitize filename to prevent path traversal
        if filename.contains("..") || filename.starts_with('/') || filename.contains('\0') {
            warn!("unsafe filename in manifest: {filename}");
            return UpdateResult::Error(format!("unsafe filename: {filename}"));
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

    // All files downloaded — save version to NVS and data partition
    if let Err(e) = write_nvs_version(nvs, &new_version) {
        warn!("failed to save version to NVS: {e}");
    }
    if let Err(e) = storage::write_file("version.txt", new_version.as_bytes()) {
        warn!("failed to save version.txt to data partition: {e}");
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

/// Delete all data files from the storage partition (catalogue + UI).
fn delete_data_files() {
    let base = storage::full_path("");
    let base_path = std::path::Path::new(&base);

    if !base_path.exists() {
        return;
    }

    // Walk the data directory and delete all files, then empty directories
    fn remove_recursive(path: &std::path::Path) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    remove_recursive(&p);
                    let _ = std::fs::remove_dir(&p);
                } else {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
    }

    remove_recursive(base_path);
    info!("cleared data partition for fresh download");
}

/// Fetch and parse the manifest.json.
fn fetch_manifest(base_url: &str) -> Result<Manifest, Box<dyn std::error::Error>> {
    let url = format!("{base_url}/manifest.json");
    let body = fetch_to_memory(&url, 8192)?;
    let manifest: Manifest = serde_json::from_slice(&body)?;
    Ok(manifest)
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
            std::io::Write::write_all(&mut file, &buf[..n])?;
            hasher.update(&buf[..n]);
            total += n as u64;
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

    let _ = std::fs::remove_file(&path);
    std::fs::rename(&temp_path, &path)?;
    Ok(())
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
pub fn installed_version(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> Option<String> {
    read_nvs_version(nvs)
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

        assert_eq!(manifest.combined_version(), "2026.04.13+ui42");
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
}

/// Get the configured updater base URL, falling back to the built-in default.
pub fn base_url(nvs: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> String {
    read_nvs_base_url(nvs)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| BASE_URL.to_string())
}

/// Persist an updater base URL override. Empty string clears the override.
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

/// Minimal SHA-256 wrapper using ESP-IDF's mbedtls.
struct Sha256 {
    ctx: sys::mbedtls_sha256_context,
}

impl Sha256 {
    fn new() -> Self {
        let mut ctx: sys::mbedtls_sha256_context = unsafe { std::mem::zeroed() };
        unsafe {
            sys::mbedtls_sha256_init(&mut ctx);
            let ret = sys::mbedtls_sha256_starts(&mut ctx, 0); // 0 = SHA-256 (not SHA-224)
            assert!(ret == 0, "mbedtls_sha256_starts failed: {ret}");
        }
        Self { ctx }
    }

    fn update(&mut self, data: &[u8]) {
        unsafe {
            let ret = sys::mbedtls_sha256_update(&mut self.ctx, data.as_ptr(), data.len());
            assert!(ret == 0, "mbedtls_sha256_update failed: {ret}");
        }
    }

    fn finish_hex(mut self) -> String {
        let mut hash = [0u8; 32];
        unsafe {
            let ret = sys::mbedtls_sha256_finish(&mut self.ctx, hash.as_mut_ptr());
            assert!(ret == 0, "mbedtls_sha256_finish failed: {ret}");
        }
        hash.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl Drop for Sha256 {
    fn drop(&mut self) {
        unsafe {
            sys::mbedtls_sha256_free(&mut self.ctx);
        }
    }
}
