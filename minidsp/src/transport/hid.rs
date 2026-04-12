//! USB HID transport using hidapi.

use super::{Transport, TransportError};
use minidsp_protocol::device::probe::is_official_usb_device;
use minidsp_protocol::packet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// Discovered USB HID device.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HidDeviceInfo {
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub product_string: Option<String>,
}

/// Enumerate all connected MiniDSP USB HID devices.
pub fn enumerate() -> Result<Vec<HidDeviceInfo>, TransportError> {
    let api = hidapi::HidApi::new().map_err(|e| TransportError::Hid(e.to_string()))?;
    let mut devices = Vec::new();

    for info in api.device_list() {
        let vid = info.vendor_id();
        let pid = info.product_id();
        if is_official_usb_device(vid, pid) {
            devices.push(HidDeviceInfo {
                path: info.path().to_string_lossy().into_owned(),
                vendor_id: vid,
                product_id: pid,
                serial_number: info.serial_number().map(|s| s.to_string()),
                product_string: info.product_string().map(|s| s.to_string()),
            });
        }
    }
    Ok(devices)
}

/// USB HID transport.
pub struct HidTransport {
    device: Arc<Mutex<hidapi::HidDevice>>,
    path: String,
    vid: u16,
    pid: u16,
}

impl HidTransport {
    /// Open a HID device by path.
    pub fn open(path: &str) -> Result<Self, TransportError> {
        let api = hidapi::HidApi::new().map_err(|e| TransportError::Hid(e.to_string()))?;
        let device = api
            .open_path(&std::ffi::CString::new(path).unwrap())
            .map_err(|e| TransportError::Hid(e.to_string()))?;
        let info = device
            .get_device_info()
            .map_err(|e| TransportError::Hid(e.to_string()))?;
        Ok(Self {
            vid: info.vendor_id(),
            pid: info.product_id(),
            device: Arc::new(Mutex::new(device)),
            path: path.to_string(),
        })
    }

    /// Open the first available MiniDSP device.
    pub fn open_first() -> Result<Self, TransportError> {
        let devices = enumerate()?;
        let info = devices.first().ok_or(TransportError::NotFound)?;
        Self::open(&info.path)
    }
}

fn map_hid_error(message: String) -> TransportError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("device disconnected")
        || lower.contains("disconnected")
        || lower.contains("no such device")
        || lower.contains("device not found")
        || lower.contains("not connected")
    {
        TransportError::Disconnected
    } else {
        TransportError::Hid(message)
    }
}

/// USB hotplug event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotplugEvent {
    /// A new MiniDSP device was connected.
    Connected(HidDeviceInfo),
    /// A MiniDSP device was disconnected (identified by path).
    Disconnected(String),
}

/// Watch for USB device connect/disconnect events by polling enumeration every 2 seconds.
///
/// Returns a `tokio::sync::watch::Receiver` that receives `Vec<HotplugEvent>` on each change.
/// The daemon can use this to add/remove devices from state.
pub fn watch_hotplug() -> (
    tokio::sync::watch::Receiver<Vec<HotplugEvent>>,
    tokio::task::JoinHandle<()>,
) {
    let (tx, rx) = tokio::sync::watch::channel(Vec::new());

    let handle = tokio::spawn(async move {
        let mut known_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

        // Initial enumeration
        if let Ok(devices) = enumerate() {
            for dev in &devices {
                known_paths.insert(dev.path.clone());
            }
        }

        let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
        loop {
            interval.tick().await;

            let current = match enumerate() {
                Ok(devices) => devices,
                Err(_) => continue,
            };

            let current_paths: std::collections::HashSet<String> =
                current.iter().map(|d| d.path.clone()).collect();

            let mut events = Vec::new();

            // New devices
            for dev in &current {
                if !known_paths.contains(&dev.path) {
                    events.push(HotplugEvent::Connected(dev.clone()));
                }
            }

            // Removed devices
            for path in &known_paths {
                if !current_paths.contains(path) {
                    events.push(HotplugEvent::Disconnected(path.clone()));
                }
            }

            if !events.is_empty() {
                let _ = tx.send(events);
            }

            known_paths = current_paths;
        }
    });

    (rx, handle)
}

impl Transport for HidTransport {
    fn send_frame(
        &self,
        payload: &[u8],
    ) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        let device = self.device.clone();
        let hid_report = packet::encode_hid(payload);
        tracing::debug!("HID OUT payload: {:02x?}", payload);
        tracing::trace!("HID OUT report: {:02x?}", hid_report);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let dev = device
                    .lock()
                    .map_err(|e| TransportError::Hid(e.to_string()))?;
                let mut retries = 10;
                loop {
                    match dev.write(&hid_report) {
                        Ok(_) => return Ok(()),
                        Err(e) if retries > 0 => {
                            retries -= 1;
                            std::thread::sleep(std::time::Duration::from_millis(250));
                            tracing::warn!("HID write retry ({retries} left): {e}");
                        }
                        Err(e) => return Err(map_hid_error(e.to_string())),
                    }
                }
            })
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?
        })
    }

    fn recv_frame(
        &self,
        timeout_ms: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, TransportError>> + Send + '_>> {
        let device = self.device.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let dev = device
                    .lock()
                    .map_err(|e| TransportError::Hid(e.to_string()))?;
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);

                loop {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        return Err(TransportError::Timeout);
                    }

                    let mut buf = [0u8; packet::HID_REPORT_SIZE];
                    let timeout = remaining.as_millis().min(i32::MAX as u128) as i32;
                    let n = dev
                        .read_timeout(&mut buf, timeout)
                        .map_err(|e| map_hid_error(e.to_string()))?;
                    if n == 0 {
                        return Err(TransportError::Timeout);
                    }
                    tracing::debug!("HID IN raw ({}): {:02x?}", n, &buf[..n]);
                    let decoded = packet::decode_hid_response(&buf[..n]);
                    if let Some(ref payload) = decoded {
                        tracing::debug!("HID IN payload: {:02x?}", payload);
                        return Ok(payload.clone());
                    }

                    tracing::warn!("HID: undecodable input packet ({} bytes), skipping", n);
                }
            })
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?
        })
    }

    fn is_connected(&self) -> bool {
        self.device.lock().is_ok()
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = Result<(), TransportError>> + Send + '_>> {
        Box::pin(async { Ok(()) })
    }

    fn url(&self) -> String {
        format!(
            "usb:{}?vid={:04x}&pid={:04x}",
            urlencoding::encode(&self.path),
            self.vid,
            self.pid,
        )
    }
}
