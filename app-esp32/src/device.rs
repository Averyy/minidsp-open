//! Lightweight MiniDSP device client for ESP32.
//!
//! Wraps the USB HID bus and protocol crate into a device-level API.
//! All methods are blocking and thread-safe (via Arc<UsbHidBus>).

use crate::usb_hid::{UsbHidBus, UsbHidError};
use log::{debug, info, warn};
use minidsp_protocol::commands::{self, HardwareId, Response};
use minidsp_protocol::device::probe;
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::dialect::{Dialect, ValueFormat};
use minidsp_protocol::eeprom;
use minidsp_protocol::status::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
struct PeqShadow {
    coeffs: [f32; 5],
    bypass: bool,
}

/// Device state — holds the USB bus, device profile, and identification info.
pub struct DeviceState {
    bus: Arc<UsbHidBus>,
    profile: DeviceProfile,
    hw_id: HardwareId,
    dsp_version: u8,
    serial: u32,
    peq_shadow: Mutex<HashMap<u16, PeqShadow>>,
}

#[derive(Debug, Serialize)]
pub struct ConfigReadResponse {
    pub inputs: Vec<InputConfigRead>,
    pub outputs: Vec<OutputConfigRead>,
}

#[derive(Debug, Serialize)]
pub struct InputConfigRead {
    pub index: u8,
    pub peq: Option<Vec<PeqConfigRead>>,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct OutputConfigRead {
    pub index: u8,
    pub peq: Option<Vec<PeqConfigRead>>,
    pub gain: Option<f32>,
    pub mute: Option<bool>,
    pub delay: Option<std::time::Duration>,
    pub invert: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct PeqConfigRead {
    pub index: u8,
    pub coeff: Option<BiquadCoeffs>,
    pub bypass: Option<bool>,
}

fn build_peq_shadow(profile: &DeviceProfile) -> HashMap<u16, PeqShadow> {
    let mut shadow = HashMap::new();
    if let Some(table) = profile.dsp_table {
        for input in table.inputs {
            for band in input.peq {
                shadow.insert(band.addr, PeqShadow {
                    coeffs: minidsp_protocol::biquad::BYPASS,
                    bypass: false,
                });
            }
        }
        for output in table.outputs {
            for band in output.peq {
                shadow.insert(band.addr, PeqShadow {
                    coeffs: minidsp_protocol::biquad::BYPASS,
                    bypass: false,
                });
            }
        }
    }
    shadow
}

impl DeviceState {
    /// Probe a device via USB HID: ReadHardwareId, read dsp_version, read serial.
    pub fn probe(bus: Arc<UsbHidBus>) -> Result<Self, UsbHidError> {
        // ReadHardwareId
        let resp = bus.command(&commands::read_hardware_id())?;
        let parsed = commands::parse_response_for_request(
            &commands::read_hardware_id(),
            &resp,
            &Dialect::NEW,
        );
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
        let req = commands::read_flash(addr_hi, addr_lo, 1);
        let resp = bus.command(&req)?;
        debug!("dsp_version raw resp ({} bytes): {:02x?}", resp.len(), &resp[..resp.len().min(10)]);
        let parsed = commands::parse_response_for_request(&req, &resp, &Dialect::NEW);
        let dsp_version = match parsed {
            Response::FlashData(ref data) => {
                debug!("dsp_version FlashData ({} bytes): {:02x?}", data.len(), data);
                eeprom::parse_dsp_version(data).unwrap_or(0)
            }
            ref other => { debug!("dsp_version response: {other:?}"); 0 }
        };
        debug!("dsp_version: {dsp_version}");

        // Read serial from EEPROM 0xFFFE
        let (addr_hi, addr_lo) = eeprom::SERIAL_ADDR;
        let req = commands::read_flash(addr_hi, addr_lo, 2);
        let resp = bus.command(&req)?;
        let parsed = commands::parse_response_for_request(&req, &resp, &Dialect::NEW);
        let serial = match parsed {
            Response::FlashData(data) => eeprom::parse_serial(&data).unwrap_or(0),
            _ => 0,
        };
        debug!("serial: {serial}");

        // Identify device
        let profile = probe::identify(hw_id.hw_id, dsp_version);
        let peq_shadow = build_peq_shadow(&profile);

        Ok(DeviceState {
            bus,
            profile,
            hw_id,
            dsp_version,
            serial,
            peq_shadow: Mutex::new(peq_shadow),
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
        let req = commands::read_flash(hi, lo, 4);
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
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
            let il = match self.read_meters(table.meters.input) {
                Ok(values) => values,
                Err(e) => {
                    warn!("input meter read failed: {e}");
                    vec![]
                }
            };
            let ol = match self.read_meters(table.meters.output) {
                Ok(values) => values,
                Err(e) => {
                    warn!("output meter read failed: {e}");
                    vec![]
                }
            };
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

    pub fn read_config(&self) -> Result<ConfigReadResponse, UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("config read unsupported".into()))?;

        let mut inputs = Vec::new();
        for (ch_idx, input) in table.inputs.iter().enumerate() {
            let mut peqs = Vec::new();
            for (band_idx, band) in input.peq.iter().enumerate() {
                match self.read_peq_coeffs(band.addr) {
                    Ok(coeffs) => {
                        peqs.push(PeqConfigRead {
                            index: band_idx as u8,
                            coeff: Some(BiquadCoeffs::from_array(&[
                                coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4],
                            ])),
                            bypass: self.read_peq_bypass(band.addr),
                        });
                    }
                    _ => {}
                }
            }

            let gain = input.gain.and_then(|addr| self.read_config_gain_db(addr).ok());
            let mute = input.mute.and_then(|addr| self.read_config_mute(addr).ok());

            inputs.push(InputConfigRead {
                index: ch_idx as u8,
                peq: Some(peqs),
                gain,
                mute,
            });
        }

        let mut outputs = Vec::new();
        for (ch_idx, output) in table.outputs.iter().enumerate() {
            let mut peqs = Vec::new();
            for (band_idx, band) in output.peq.iter().enumerate() {
                match self.read_peq_coeffs(band.addr) {
                    Ok(coeffs) => {
                        peqs.push(PeqConfigRead {
                            index: band_idx as u8,
                            coeff: Some(BiquadCoeffs::from_array(&[
                                coeffs[0], coeffs[1], coeffs[2], coeffs[3], coeffs[4],
                            ])),
                            bypass: self.read_peq_bypass(band.addr),
                        });
                    }
                    _ => {}
                }
            }

            let gain = self.read_config_gain_db(output.gain).ok();
            let mute = self.read_config_mute(output.mute).ok();
            let delay = output.delay.and_then(|addr| self.read_config_delay(addr).ok());
            let invert = output
                .polarity
                .and_then(|addr| self.read_config_invert(addr).ok());

            outputs.push(OutputConfigRead {
                index: ch_idx as u8,
                peq: Some(peqs),
                gain,
                mute,
                delay,
                invert,
            });
        }

        Ok(ConfigReadResponse { inputs, outputs })
    }

    /// Read float values from DSP memory starting at `addr`.
    fn read_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::Floats(values) => Ok(values),
            other => Err(UsbHidError::Protocol(format!(
                "read_floats: {other:?}"
            ))),
        }
    }

    fn read_param_words(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        commands::parse_param_words_response_for_request(&req, &resp)
            .ok_or_else(|| UsbHidError::Protocol("typed read_floats parse failed".into()))
    }

    fn read_param_word(&self, addr: u16) -> Result<u32, UsbHidError> {
        self.read_param_words(addr, 1)?
            .into_iter()
            .next()
            .ok_or_else(|| UsbHidError::Protocol("empty typed param response".into()))
    }

    fn read_peq_coeffs(&self, addr: u16) -> Result<[f32; 5], UsbHidError> {
        let values = self.read_floats(addr, 5).unwrap_or_default();
        if values.len() >= 5 {
            let coeffs = [values[0], values[1], values[2], values[3], values[4]];
            if coeffs != [0.0; 5] {
                return Ok(coeffs);
            }
        }
        Ok(self.peq_shadow.lock()
            .map_err(|_| UsbHidError::Protocol("peq shadow poisoned".into()))?
            .get(&addr)
            .map(|s| s.coeffs)
            .unwrap_or(minidsp_protocol::biquad::BYPASS))
    }

    fn read_peq_bypass(&self, addr: u16) -> Option<bool> {
        self.peq_shadow.lock().ok()?.get(&addr).map(|s| s.bypass)
    }

    fn read_config_gain_db(&self, addr: u16) -> Result<f32, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(f32::from_bits(self.read_param_word(addr)?)),
            ValueFormat::FixedPoint => {
                let linear = self.read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty gain response".into()))?;
                Ok(20.0 * linear.max(1e-10).log10())
            }
        }
    }

    fn read_config_mute(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_param_word(addr)? >= 2),
            ValueFormat::FixedPoint => {
                let value = self.read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty mute response".into()))?;
                Ok(value == 0.0)
            }
        }
    }

    fn read_config_delay(&self, addr: u16) -> Result<std::time::Duration, UsbHidError> {
        let samples = match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.read_param_word(addr)? as f32,
            ValueFormat::FixedPoint => self.read_floats(addr, 1)?
                .into_iter()
                .next()
                .ok_or_else(|| UsbHidError::Protocol("empty delay response".into()))?,
        };
        Ok(std::time::Duration::from_secs_f32(samples / self.profile.sample_rate as f32))
    }

    fn read_config_invert(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_param_word(addr)? != 0),
            ValueFormat::FixedPoint => {
                let value = self.read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty invert response".into()))?;
                Ok(value < 0.0)
            }
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
        info!("write_biquad start addr=0x{addr:04x}");
        let save = matches!(self.profile.dialect.value_format, ValueFormat::FixedPoint);
        for (offset, coeff) in coeffs.iter().enumerate() {
            self.write_dsp(addr + offset as u16, *coeff, save)?;
        }
        info!("write_biquad done addr=0x{addr:04x}");
        if let Ok(mut shadow) = self.peq_shadow.lock() {
            let entry = shadow.entry(addr).or_insert(PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            });
            entry.coeffs = *coeffs;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn bypass_filter(&self, addr: u16, bypass: bool) -> Result<(), UsbHidError> {
        info!("bypass_filter start addr=0x{addr:04x} bypass={bypass}");
        self.bus
            .command_no_reply(&commands::bypass_filter(addr, bypass, self.dialect()))?;
        info!("bypass_filter done addr=0x{addr:04x} bypass={bypass}");
        if let Ok(mut shadow) = self.peq_shadow.lock() {
            let entry = shadow.entry(addr).or_insert(PeqShadow {
                coeffs: minidsp_protocol::biquad::BYPASS,
                bypass: false,
            });
            entry.bypass = bypass;
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_dsp(&self, addr: u16, value: f32, save: bool) -> Result<(), UsbHidError> {
        self.bus
            .command(&commands::write_dsp(addr, value, save, self.dialect()))?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn write_dsp_u32(&self, addr: u16, value: u32, save: bool) -> Result<(), UsbHidError> {
        self.bus
            .command(&commands::write_dsp_u32(addr, value, save))?;
        Ok(())
    }

    /// Set channel polarity inversion using dialect-aware encoding.
    #[allow(dead_code)]
    pub fn set_invert(&self, addr: u16, inverted: bool) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_u32(addr, if inverted { 1 } else { 0 }, false),
            ValueFormat::FixedPoint => {
                let val = minidsp_protocol::dialect::polarity_value(inverted, self.dialect());
                self.write_dsp(addr, val, true)
            }
        }
    }

    #[allow(dead_code)]
    pub fn set_channel_gain(&self, addr: u16, db: f32) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp(addr, db, false),
            ValueFormat::FixedPoint => self.write_dsp(addr, 10.0f32.powf(db / 20.0), true),
        }
    }

    #[allow(dead_code)]
    pub fn set_channel_mute(&self, addr: u16, muted: bool) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_u32(addr, if muted { 2 } else { 1 }, false),
            ValueFormat::FixedPoint => {
                let val = if muted { 0.0 } else { 1.0 };
                self.write_dsp(addr, val, true)
            }
        }
    }

    #[allow(dead_code)]
    pub fn set_channel_delay_samples(&self, addr: u16, samples: u32) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_u32(addr, samples, false),
            ValueFormat::FixedPoint => self.write_dsp(addr, samples as f32, true),
        }
    }

    // -- Flash reads --

    pub fn read_flash_data(
        &self,
        addr_hi: u8,
        addr_lo: u8,
        count: u8,
    ) -> Result<Vec<u8>, UsbHidError> {
        let req = commands::read_flash(addr_hi, addr_lo, count);
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
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
