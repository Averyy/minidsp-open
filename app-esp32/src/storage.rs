//! LittleFS data partition for ezBEQ catalogue, UI assets, and config.
//!
//! Mounts the "storage" partition at `/data` via ESP-IDF VFS.
//! After mounting, all file I/O uses standard `std::fs`.

#![allow(dead_code)] // Public API — used by upcoming ezBEQ modules

use esp_idf_svc::sys;
use log::info;
use std::ffi::CString;

const PARTITION_LABEL: &str = "storage";
const MOUNT_POINT: &str = "/data";

/// Partition usage info.
pub struct StorageInfo {
    pub total_bytes: usize,
    pub used_bytes: usize,
}

/// Mount the LittleFS data partition at `/data`.
///
/// Formats the partition on first boot (empty/corrupt flash).
/// Returns an error if the partition is missing or mount fails after format.
pub fn mount() -> Result<StorageInfo, Box<dyn std::error::Error>> {
    let base_path = CString::new(MOUNT_POINT)?;
    let partition_label = CString::new(PARTITION_LABEL)?;

    // Use raw FFI for format_if_mount_failed (esp-idf-svc safe wrapper doesn't expose this).
    // Bitfields are packed by bindgen — use setter methods.
    let mut conf = sys::esp_vfs_littlefs_conf_t {
        base_path: base_path.as_ptr(),
        partition_label: partition_label.as_ptr(),
        ..Default::default()
    };
    conf.set_format_if_mount_failed(1); // Format on first boot
    conf.set_grow_on_mount(1); // Expand filesystem to fill partition

    unsafe {
        let ret = sys::esp_vfs_littlefs_register(&conf);
        if ret != sys::ESP_OK {
            return Err(format!("LittleFS mount failed: ESP error {ret}").into());
        }
    }

    let info = partition_info(&partition_label)?;
    info!(
        "LittleFS mounted at {MOUNT_POINT}: {:.1}MB total, {:.1}KB used",
        info.total_bytes as f64 / (1024.0 * 1024.0),
        info.used_bytes as f64 / 1024.0,
    );

    Ok(info)
}

/// Query partition usage.
pub fn info() -> Result<StorageInfo, Box<dyn std::error::Error>> {
    let partition_label = CString::new(PARTITION_LABEL)?;
    partition_info(&partition_label)
}

fn partition_info(label: &CString) -> Result<StorageInfo, Box<dyn std::error::Error>> {
    let mut total: usize = 0;
    let mut used: usize = 0;
    unsafe {
        let ret = sys::esp_littlefs_info(label.as_ptr(), &mut total, &mut used);
        if ret != sys::ESP_OK {
            return Err(format!("LittleFS info failed: ESP error {ret}").into());
        }
    }
    Ok(StorageInfo {
        total_bytes: total,
        used_bytes: used,
    })
}

/// Check if a file exists on the data partition.
pub fn file_exists(path: &str) -> bool {
    let full = format!("{MOUNT_POINT}/{path}");
    std::path::Path::new(&full).exists()
}

/// Read a file from the data partition. Returns None if not found.
pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let full = format!("{MOUNT_POINT}/{path}");
    std::fs::read(&full).ok()
}

/// Write a file to the data partition, creating parent directories as needed.
pub fn write_file(path: &str, data: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let full = format!("{MOUNT_POINT}/{path}");
    if let Some(parent) = std::path::Path::new(&full).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&full, data)?;
    Ok(())
}

/// Full path on the VFS for a relative path within the data partition.
pub fn full_path(relative: &str) -> String {
    format!("{MOUNT_POINT}/{relative}")
}
