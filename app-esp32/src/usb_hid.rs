//! USB Host HID transport for ESP32-S3.
//!
//! Uses esp-idf-sys FFI to the ESP-IDF USB Host HID driver.
//! This is ~200 lines of unsafe code wrapping the C API.

use esp_idf_svc::sys;
use log::{info, warn, error};
use core::ffi::c_void;
use core::ptr;

/// MiniDSP USB VID.
const MINIDSP_VID: u16 = 0x2752;

/// HID report size (1 byte report ID + 64 bytes data).
const HID_REPORT_SIZE: usize = 65;

/// Discovered MiniDSP device via USB Host HID.
pub struct MiniDspDevice {
    pub hw_id: u8,
    pub fw_major: u8,
    pub fw_minor: u8,
    pub dsp_version: u8,
    pub serial: u32,
    pub vid: u16,
    pub pid: u16,
}

/// Error type for USB HID operations.
#[derive(Debug)]
pub enum UsbHidError {
    Init(String),
    DeviceNotFound,
    Write(String),
    Read(String),
    Timeout,
}

impl core::fmt::Display for UsbHidError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            UsbHidError::Init(s) => write!(f, "USB init: {s}"),
            UsbHidError::DeviceNotFound => write!(f, "no MiniDSP device found"),
            UsbHidError::Write(s) => write!(f, "USB write: {s}"),
            UsbHidError::Read(s) => write!(f, "USB read: {s}"),
            UsbHidError::Timeout => write!(f, "USB timeout"),
        }
    }
}

/// Initialize USB Host and find a MiniDSP device.
///
/// This function:
/// 1. Initializes the USB Host Library
/// 2. Installs the HID Host driver
/// 3. Waits for a device with VID 0x2752 to connect
/// 4. Probes the device (ReadHardwareId + ReadFlash for dsp_version)
/// 5. Returns device info
pub async fn init_and_find_device() -> Result<MiniDspDevice, UsbHidError> {
    // TODO: Implement USB Host HID initialization
    // This requires unsafe FFI to:
    //   - usb_host_install()
    //   - usb_host_lib_handle_events() in a loop
    //   - hid_host_install()
    //   - hid_host_device_open() on device connect callback
    //   - hid_host_device_get_raw_input_report_queue()
    //
    // For now, return a placeholder error.
    // The actual implementation follows the pattern in:
    //   ~/code/esp-uac2-host/simulators/minidsp-2x4hd/ (device side)
    //   ~/code/esp-uac2-host/components/uac2_host/ (host side reference)
    //
    // The HID interface is interface 2 on the miniDSP composite device.
    // We need to:
    //   1. Claim interface 2
    //   2. Find the HID IN and OUT endpoints
    //   3. Send/receive 65-byte reports (report ID 0x00 + 64 bytes)

    info!("USB Host HID: waiting for MiniDSP device (VID 0x{MINIDSP_VID:04X})...");

    // Placeholder: in the real implementation, this would block until
    // a device is enumerated and probed
    Err(UsbHidError::DeviceNotFound)
}

/// Send a command payload to the device and receive the response.
///
/// This wraps the payload in a HID report (report ID 0x00 + frame + 0xFF padding)
/// and waits for the response.
pub async fn hid_roundtrip(payload: &[u8]) -> Result<Vec<u8>, UsbHidError> {
    // Encode as HID report
    let report = minidsp_protocol::packet::encode_hid(payload);

    // TODO: Send report via usb_host_transfer_submit()
    // TODO: Wait for response via HID input report callback
    // TODO: Decode HID report → frame → payload

    Err(UsbHidError::DeviceNotFound)
}
