//! Lightweight MiniDSP device client for ESP32.
//!
//! Wraps the USB HID bus and protocol crate into a device-level API.
//! All methods are blocking and thread-safe (via Arc<UsbHidBus>).

use crate::usb_hid::{UsbHidBus, UsbHidError};
use log::debug;
use minidsp_protocol::commands::{self, HardwareId, Response};
use minidsp_protocol::device::probe;
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::dialect::Dialect;
use minidsp_protocol::eeprom;
use minidsp_protocol::status::*;
use std::sync::Arc;

/// Device state — holds the USB bus, device profile, and identification info.
pub struct DeviceState {
    bus: Arc<UsbHidBus>,
    profile: DeviceProfile,
    hw_id: HardwareId,
    dsp_version: u8,
    serial: u32,
}

impl DeviceState {
    /// Probe a device via USB HID: ReadHardwareId, read dsp_version, read serial.
    pub fn probe(bus: Arc<UsbHidBus>) -> Result<Self, UsbHidError> {
        // ReadHardwareId
        let resp = bus.command(&commands::read_hardware_id())?;
        let parsed = commands::parse_response(&resp, &Dialect::NEW);
        let hw_id = match parsed {
            Response::HardwareId(id) => id,
            other => {
                return Err(UsbHidError::Protocol(format!(
                    "expected HardwareId, got {other:?}"
                )))
            }
        };
        debug!(
            "hw_id: fw={}.{}, hw={}",
            hw_id.fw_major, hw_id.fw_minor, hw_id.hw_id
        );

        // Read DSP version from EEPROM 0xFFA1
        let (addr_hi, addr_lo) = eeprom::DSP_VERSION_ADDR;
        let resp = bus.command(&commands::read_flash(addr_hi, addr_lo, 1))?;
        let parsed = commands::parse_response(&resp, &Dialect::NEW);
        let dsp_version = match parsed {
            Response::FlashData(data) => eeprom::parse_dsp_version(&data).unwrap_or(0),
            _ => 0,
        };
        debug!("dsp_version: {dsp_version}");

        // Read serial from EEPROM 0xFFFE
        let (addr_hi, addr_lo) = eeprom::SERIAL_ADDR;
        let resp = bus.command(&commands::read_flash(addr_hi, addr_lo, 2))?;
        let parsed = commands::parse_response(&resp, &Dialect::NEW);
        let serial = match parsed {
            Response::FlashData(data) => eeprom::parse_serial(&data).unwrap_or(0),
            _ => 0,
        };
        debug!("serial: {serial}");

        // Identify device
        let profile = probe::identify(hw_id.hw_id, dsp_version);

        Ok(DeviceState {
            bus,
            profile,
            hw_id,
            dsp_version,
            serial,
        })
    }

    // -- Accessors --

    pub fn hw_id(&self) -> &HardwareId {
        &self.hw_id
    }
    pub fn dsp_version(&self) -> u8 {
        self.dsp_version
    }
    pub fn serial(&self) -> u32 {
        self.serial
    }
    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }
    pub fn product_name(&self) -> &str {
        probe::product_name(self.hw_id.hw_id, self.dsp_version)
    }
    fn dialect(&self) -> &Dialect {
        &self.profile.dialect
    }

    // -- Master status --

    pub fn get_master_status(&self) -> Result<eeprom::MasterStatusRaw, UsbHidError> {
        let (hi, lo) = eeprom::MASTER_STATUS_ADDR;
        let resp = self.bus.command(&commands::read_flash(hi, lo, 4))?;
        match commands::parse_response(&resp, self.dialect()) {
            Response::FlashData(data) if data.len() >= 4 => {
                eeprom::MasterStatusRaw::from_bytes(&data)
                    .ok_or_else(|| UsbHidError::Protocol("bad master status".into()))
            }
            other => Err(UsbHidError::Protocol(format!(
                "master status: {other:?}"
            ))),
        }
    }

    pub fn get_status_summary(&self) -> Result<StatusSummary, UsbHidError> {
        let ms = self.get_master_status()?;
        let source_name = self
            .profile
            .sources
            .source_for_index(ms.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Source{}", ms.source));

        let dirac = if self.profile.has_dirac {
            self.read_dirac_state().ok()
        } else {
            None
        };

        // Read level meters if DSP table is available
        let (input_levels, output_levels) = if let Some(table) = self.profile.dsp_table {
            let il = self.read_meters(table.meters.input).unwrap_or_default();
            let ol = self.read_meters(table.meters.output).unwrap_or_default();
            (il, ol)
        } else {
            (vec![], vec![])
        };

        Ok(StatusSummary {
            master: MasterStatusView {
                preset: ms.preset,
                source: source_name,
                volume: ms.volume_db(),
                mute: ms.is_muted(),
                dirac,
            },
            input_levels,
            output_levels,
        })
    }

    /// Read float values from DSP memory starting at `addr`.
    fn read_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, UsbHidError> {
        let resp = self
            .bus
            .command(&commands::read_floats(addr, count, self.dialect()))?;
        match commands::parse_response(&resp, self.dialect()) {
            Response::Floats(values) => Ok(values),
            other => Err(UsbHidError::Protocol(format!(
                "read_floats: {other:?}"
            ))),
        }
    }

    /// Read meter values from an array of DSP addresses and convert to dB.
    /// Batches contiguous addresses into single CMD_READ_FLOATS calls (max 14 per call).
    fn read_meters(&self, addrs: &[u16]) -> Result<Vec<f32>, UsbHidError> {
        if addrs.is_empty() {
            return Ok(vec![]);
        }

        let mut all_values = Vec::with_capacity(addrs.len());

        // Check if addresses are contiguous for a single bulk read
        let is_contiguous =
            addrs.len() <= 14 && addrs.windows(2).all(|w| w[1] == w[0] + 1);

        if is_contiguous {
            let values = self.read_floats(addrs[0], addrs.len() as u8)?;
            all_values.extend(values);
        } else {
            // Find contiguous runs and read each in bulk
            let mut i = 0;
            while i < addrs.len() {
                let start = i;
                while i + 1 < addrs.len()
                    && addrs[i + 1] == addrs[i] + 1
                    && (i - start + 1) < 14
                {
                    i += 1;
                }
                let count = (i - start + 1).min(14);
                let values = self.read_floats(addrs[start], count as u8)?;
                all_values.extend(values);
                i += 1;
            }
        }

        // Convert raw float to dB: 20 * log10(abs(value)), clamped to -140 dB
        let db_values: Vec<f32> = all_values
            .iter()
            .map(|&v| {
                if v.abs() < 1e-10 {
                    -140.0
                } else {
                    20.0 * v.abs().log10()
                }
            })
            .collect();
        Ok(db_values)
    }

    // -- Control commands --

    pub fn set_volume(&self, value: u8) -> Result<(), UsbHidError> {
        self.bus.command(&commands::set_volume(value))?;
        Ok(())
    }

    pub fn set_volume_db(&self, db: f32) -> Result<(), UsbHidError> {
        let value = ((-db) * 2.0).round().max(0.0).min(255.0) as u8;
        self.set_volume(value)
    }

    pub fn set_mute(&self, muted: bool) -> Result<(), UsbHidError> {
        self.bus.command(&commands::set_mute(muted))?;
        Ok(())
    }

    pub fn set_source(&self, source: u8) -> Result<(), UsbHidError> {
        self.bus.command(&commands::set_source(source))?;
        Ok(())
    }

    pub fn set_preset(&self, preset: u8) -> Result<(), UsbHidError> {
        self.bus.command(&commands::set_config(preset, 0))?;
        Ok(())
    }

    pub fn set_dirac_bypass(&self, bypass: bool) -> Result<(), UsbHidError> {
        self.bus.command(&commands::dirac_bypass(bypass))?;
        Ok(())
    }

    pub fn set_display_brightness(&self, level: u8) -> Result<(), UsbHidError> {
        self.bus
            .command(&commands::set_display_brightness(level))?;
        Ok(())
    }

    pub fn set_display_idle_time(&self, secs: u8) -> Result<(), UsbHidError> {
        self.bus
            .command(&commands::set_display_idle_time(secs))?;
        Ok(())
    }

    pub fn set_dre(&self, enabled: bool) -> Result<(), UsbHidError> {
        self.bus.command(&commands::set_dre(enabled))?;
        Ok(())
    }

    pub fn copy_preset(&self) -> Result<(), UsbHidError> {
        self.bus.command(&commands::copy_preset(true))?;
        Ok(())
    }

    // -- DSP writes --

    #[allow(dead_code)]
    pub fn write_biquad(&self, addr: u16, coeffs: &[f32; 5]) -> Result<(), UsbHidError> {
        self.bus
            .command(&commands::write_biquad(addr, coeffs, self.dialect()))?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn bypass_filter(&self, addr: u16, bypass: bool) -> Result<(), UsbHidError> {
        self.bus
            .command(&commands::bypass_filter(addr, bypass, self.dialect()))?;
        Ok(())
    }

    // -- Flash reads --

    pub fn read_flash_data(
        &self,
        addr_hi: u8,
        addr_lo: u8,
        count: u8,
    ) -> Result<Vec<u8>, UsbHidError> {
        let resp = self
            .bus
            .command(&commands::read_flash(addr_hi, addr_lo, count))?;
        match commands::parse_response(&resp, self.dialect()) {
            Response::FlashData(data) => Ok(data),
            other => Err(UsbHidError::Protocol(format!("flash read: {other:?}"))),
        }
    }

    // -- Display (EEPROM 0xFFE8) --

    pub fn read_display(&self) -> Result<(u8, u8), UsbHidError> {
        let data = self.read_flash_data(0xFF, 0xE8, 2)?;
        Ok((
            data.first().copied().unwrap_or(0), // idle_time
            data.get(1).copied().unwrap_or(0),  // brightness
        ))
    }

    // -- DRE (EEPROM 0xFFED) --

    pub fn read_dre(&self) -> Result<bool, UsbHidError> {
        let data = self.read_flash_data(0xFF, 0xED, 1)?;
        Ok(data.first().copied().unwrap_or(0) != 0)
    }

    // -- Dirac (EEPROM 0xFFE0) --

    pub fn read_dirac_state(&self) -> Result<bool, UsbHidError> {
        let data = self.read_flash_data(0xFF, 0xE0, 1)?;
        Ok(data.first().copied().unwrap_or(1) == 0) // 0 = active, 1 = bypassed
    }

    // -- Tokens (EEPROM 0xFFC8) --

    pub fn read_tokens(&self) -> Result<Vec<u8>, UsbHidError> {
        let (hi, lo) = eeprom::TOKENS_ADDR;
        self.read_flash_data(hi, lo, 8)
    }

    // -- Raw command --

    #[allow(dead_code)]
    pub fn raw_command(&self, payload: &[u8]) -> Result<Vec<u8>, UsbHidError> {
        self.bus.command(payload)
    }

    // -- Device info --

    pub fn device_info(&self) -> DeviceInfo {
        DeviceInfo {
            url: format!("usb://0x{:04x}:0x{:04x}", self.bus.vid(), self.bus.pid()),
            version: DeviceVersion {
                hw_id: self.hw_id.hw_id,
                fw_major: self.hw_id.fw_major,
                fw_minor: self.hw_id.fw_minor,
                dsp_version: self.dsp_version,
                serial: self.serial,
            },
            product_name: self.product_name().to_string(),
        }
    }
}
