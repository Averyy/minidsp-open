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
use minidsp_protocol::source::Source;
use minidsp_protocol::status::*;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

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
    operation_lock: Mutex<()>,
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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub routing: Vec<RoutingConfigRead>,
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

#[derive(Debug, Serialize)]
pub struct RoutingConfigRead {
    pub output: u8,
    pub enabled: bool,
    pub gain_db: f32,
}

fn build_peq_shadow(profile: &DeviceProfile) -> HashMap<u16, PeqShadow> {
    let mut shadow = HashMap::new();
    if let Some(table) = profile.dsp_table {
        for input in table.inputs {
            for band in input.peq {
                shadow.insert(
                    band.addr,
                    PeqShadow {
                        coeffs: minidsp_protocol::biquad::BYPASS,
                        bypass: false,
                    },
                );
            }
        }
        for output in table.outputs {
            for band in output.peq {
                shadow.insert(
                    band.addr,
                    PeqShadow {
                        coeffs: minidsp_protocol::biquad::BYPASS,
                        bypass: false,
                    },
                );
            }
        }
    }
    shadow
}

/// Timeout for slow commands that write to flash (preset copy, set_config).
const SLOW_COMMAND_TIMEOUT: Duration = Duration::from_secs(35);

impl DeviceState {
    fn expect_write_ok(&self, payload: &[u8], context: &str) -> Result<(), UsbHidError> {
        let resp = self.bus.command(payload)?;
        match commands::parse_response_for_request(payload, &resp, self.dialect()) {
            Response::Ack | Response::PresetComplete | Response::ResetComplete => Ok(()),
            Response::Error(data) => Err(UsbHidError::Protocol(format!(
                "{context}: device error {data:02x?}"
            ))),
            other => Err(UsbHidError::Protocol(format!(
                "{context}: unexpected response {other:?}"
            ))),
        }
    }

    fn expect_write_ok_slow(&self, payload: &[u8], context: &str) -> Result<(), UsbHidError> {
        let resp = self.bus.command_with_timeout(payload, SLOW_COMMAND_TIMEOUT)?;
        match commands::parse_response_for_request(payload, &resp, self.dialect()) {
            Response::Ack | Response::PresetComplete | Response::ResetComplete => Ok(()),
            Response::Error(data) => Err(UsbHidError::Protocol(format!(
                "{context}: device error {data:02x?}"
            ))),
            other => Err(UsbHidError::Protocol(format!(
                "{context}: unexpected response {other:?}"
            ))),
        }
    }

    pub fn probe_with_timeout(bus: Arc<UsbHidBus>, timeout: Duration) -> Result<Self, UsbHidError> {
        // ReadHardwareId
        let resp = bus.command_with_timeout(&commands::read_hardware_id(), timeout)?;
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
        let resp = bus.command_with_timeout(&req, timeout)?;
        debug!(
            "dsp_version raw resp ({} bytes): {:02x?}",
            resp.len(),
            &resp[..resp.len().min(10)]
        );
        let parsed = commands::parse_response_for_request(&req, &resp, &Dialect::NEW);
        let dsp_version = match parsed {
            Response::FlashData(ref data) => {
                debug!(
                    "dsp_version FlashData ({} bytes): {:02x?}",
                    data.len(),
                    data
                );
                eeprom::parse_dsp_version(data).unwrap_or(0)
            }
            ref other => {
                debug!("dsp_version response: {other:?}");
                0
            }
        };
        debug!("dsp_version: {dsp_version}");

        // Read serial from EEPROM 0xFFFE
        let (addr_hi, addr_lo) = eeprom::SERIAL_ADDR;
        let req = commands::read_flash(addr_hi, addr_lo, 2);
        let resp = bus.command_with_timeout(&req, timeout)?;
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
            operation_lock: Mutex::new(()),
            peq_shadow: Mutex::new(peq_shadow),
        })
    }

    /// Serialize whole logical operations so multi-command reads/writes cannot
    /// interleave with background polling or bridge traffic.
    pub fn begin_operation(&self) -> std::sync::MutexGuard<'_, ()> {
        self.operation_lock
            .lock()
            .unwrap_or_else(|err| err.into_inner())
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

    pub fn current_source(&self) -> Result<Option<Source>, UsbHidError> {
        let status = self.get_master_status()?;
        Ok(self.profile.sources.source_for_index(status.source))
    }
    fn dialect(&self) -> &Dialect {
        &self.profile.dialect
    }

    // -- Master status --

    pub fn get_master_status(&self) -> Result<eeprom::MasterStatusRaw, UsbHidError> {
        const MASTER_STATUS_RETRIES: usize = 6;
        const MASTER_STATUS_RETRY_DELAY: Duration = Duration::from_millis(100);

        let mut last_err = None;
        for attempt in 0..MASTER_STATUS_RETRIES {
            match self.read_master_status_once() {
                Ok(status) => return Ok(status),
                Err(UsbHidError::Protocol(err)) => {
                    last_err = Some(err);
                    if attempt + 1 < MASTER_STATUS_RETRIES {
                        std::thread::sleep(MASTER_STATUS_RETRY_DELAY);
                    }
                }
                Err(err) => return Err(err),
            }
        }

        Err(UsbHidError::Protocol(
            last_err.unwrap_or_else(|| "master status retry exhausted".into()),
        ))
    }

    fn read_master_status_once(&self) -> Result<eeprom::MasterStatusRaw, UsbHidError> {
        let (hi, lo) = eeprom::MASTER_STATUS_ADDR;
        let req = commands::read_flash(hi, lo, 4);
        let resp = self.bus.command(&req)?;
        match commands::parse_response_for_request(&req, &resp, self.dialect()) {
            Response::FlashData(data) if data.len() >= 4 => {
                eeprom::MasterStatusRaw::from_bytes(&data)
                    .ok_or_else(|| UsbHidError::Protocol("bad master status".into()))
            }
            other => Err(UsbHidError::Protocol(format!("master status: {other:?}"))),
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

            let gain = input
                .gain
                .and_then(|addr| self.read_config_gain_db(addr).ok());
            let mute = input.mute.and_then(|addr| self.read_config_mute(addr).ok());
            let mut routing = Vec::new();
            let matrix = table.routing;
            if ch_idx < matrix.rows as usize {
                for output in 0..matrix.cols {
                    let flat_idx = ch_idx as u16 * matrix.cols as u16 + output as u16;
                    let status_addr = matrix.status_base + flat_idx;
                    let gain_addr = matrix.gain_base + flat_idx;
                    let status = self.read_param_word(status_addr).unwrap_or(0);
                    // Mixer status readback via 0x14 is boolean-collapsed on
                    // devices with unreliable selector readback: write values
                    // 2 (enabled) and 1 (disabled) both read back as 1.
                    // Treat non-zero as enabled on all dialects.
                    let enabled = status != 0;
                    let gain_db = self.read_config_gain_db(gain_addr).unwrap_or(0.0);
                    routing.push(RoutingConfigRead {
                        output,
                        enabled,
                        gain_db,
                    });
                }
            }

            inputs.push(InputConfigRead {
                index: ch_idx as u8,
                peq: Some(peqs),
                gain,
                mute,
                routing,
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
            let delay = output
                .delay
                .and_then(|addr| self.read_config_delay(addr).ok());
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
            other => Err(UsbHidError::Protocol(format!("read_floats: {other:?}"))),
        }
    }

    pub fn read_dsp_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, UsbHidError> {
        self.read_floats(addr, count)
    }

    fn read_param_words(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        commands::parse_param_words_response_for_request(&req, &resp)
            .ok_or_else(|| UsbHidError::Protocol("typed read_floats parse failed".into()))
    }

    fn read_param_words_be(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        let req = commands::read_floats(addr, count, self.dialect());
        let resp = self.bus.command(&req)?;
        commands::parse_param_words_be_response_for_request(&req, &resp).ok_or_else(|| {
            UsbHidError::Protocol("big-endian typed read_floats parse failed".into())
        })
    }

    pub fn read_dsp_words(&self, addr: u16, count: u8) -> Result<Vec<u32>, UsbHidError> {
        self.read_param_words(addr, count)
    }

    fn read_param_word(&self, addr: u16) -> Result<u32, UsbHidError> {
        self.read_param_words(addr, 1)?
            .into_iter()
            .next()
            .ok_or_else(|| UsbHidError::Protocol("empty typed param response".into()))
    }

    fn read_peq_coeffs(&self, addr: u16) -> Result<[f32; 5], UsbHidError> {
        // On devices with unreliable selector readback (DSP100), always prefer
        // shadow state — live reads can return collapsed/bogus values.
        if self.profile.has_unreliable_selector_readback {
            return Ok(self
                .peq_shadow
                .lock()
                .map_err(|_| UsbHidError::Protocol("peq shadow poisoned".into()))?
                .get(&addr)
                .map(|s| s.coeffs)
                .unwrap_or(minidsp_protocol::biquad::BYPASS));
        }

        let values = self.read_floats(addr, 5).unwrap_or_default();
        if values.len() >= 5 {
            let coeffs = [values[0], values[1], values[2], values[3], values[4]];
            if coeffs != [0.0; 5] {
                return Ok(coeffs);
            }
        }
        // Fallback to shadow for any device if live read returned all zeros
        Ok(self
            .peq_shadow
            .lock()
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
                let linear = self
                    .read_floats(addr, 1)?
                    .into_iter()
                    .next()
                    .ok_or_else(|| UsbHidError::Protocol("empty gain response".into()))?;
                Ok(20.0 * linear.max(1e-10).log10())
            }
        }
    }

    fn read_config_mute(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_param_word(addr)? == 1),
            ValueFormat::FixedPoint => {
                let value = self
                    .read_floats(addr, 1)?
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
            ValueFormat::FixedPoint => self
                .read_param_words_be(addr, 1)?
                .into_iter()
                .next()
                .ok_or_else(|| UsbHidError::Protocol("empty delay response".into()))?
                as f32,
        };
        Ok(std::time::Duration::from_secs_f32(
            samples / self.profile.sample_rate as f32,
        ))
    }

    fn read_config_invert(&self, addr: u16) -> Result<bool, UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => Ok(self.read_param_word(addr)? != 0),
            ValueFormat::FixedPoint => {
                let value = self
                    .read_floats(addr, 1)?
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
        let is_contiguous = addrs.len() <= 14 && addrs.windows(2).all(|w| w[1] == w[0] + 1);

        if is_contiguous {
            let values = self.read_floats(addrs[0], addrs.len() as u8)?;
            all_values.extend(values);
        } else {
            // Find contiguous runs and read each in bulk
            let mut i = 0;
            while i < addrs.len() {
                let start = i;
                while i + 1 < addrs.len() && addrs[i + 1] == addrs[i] + 1 && (i - start + 1) < 14 {
                    i += 1;
                }
                let count = (i - start + 1).min(14);
                let values = self.read_floats(addrs[start], count as u8)?;
                all_values.extend(values);
                i += 1;
            }
        }

        // SHARC-family meter registers already report dB-like float values.
        // Re-applying log10() here turns valid negative readings like -120 dB
        // into misleading positive numbers (~41.58). Return the raw meter
        // values and only clamp pathological silence / NaN cases.
        let db_values: Vec<f32> = all_values
            .iter()
            .map(|&v| {
                if !v.is_finite() {
                    -140.0
                } else if v <= -140.0 || v.abs() < 1e-10 {
                    -140.0
                } else {
                    v
                }
            })
            .collect();
        Ok(db_values)
    }

    // -- Control commands --

    pub fn set_volume(&self, value: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_volume(value), "set volume")
    }

    pub fn set_volume_db(&self, db: f32) -> Result<(), UsbHidError> {
        let value = ((-db) * 2.0).round().max(0.0).min(255.0) as u8;
        self.set_volume(value)
    }

    pub fn set_mute(&self, muted: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_mute(muted), "set mute")
    }

    pub fn set_source(&self, source: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_source(source), "set source")
    }

    pub fn set_preset(&self, preset: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok_slow(&commands::set_config(preset, 0), "set preset")
    }

    pub fn set_dirac_bypass(&self, bypass: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::dirac_bypass(bypass), "set dirac bypass")
    }

    pub fn set_display_brightness(&self, level: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::set_display_brightness(level),
            "set display brightness",
        )
    }

    pub fn set_display_idle_time(&self, secs: u8) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::set_display_idle_time(secs),
            "set display idle time",
        )
    }

    pub fn set_dre(&self, enabled: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::set_dre(enabled), "set dre")
    }

    pub fn copy_preset(&self) -> Result<(), UsbHidError> {
        self.expect_write_ok_slow(&commands::copy_preset(true), "copy preset")
    }

    // -- DSP writes --

    #[allow(dead_code)]
    pub fn write_biquad(&self, addr: u16, coeffs: &[f32; 5]) -> Result<(), UsbHidError> {
        info!("write_biquad start addr=0x{addr:04x}");
        self.expect_write_ok(
            &commands::write_biquad(addr, coeffs, self.dialect()),
            "write biquad",
        )?;
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
        self.expect_write_ok(
            &commands::bypass_filter(addr, bypass, self.dialect()),
            "bypass filter",
        )?;
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
        self.expect_write_ok(
            &commands::write_dsp(addr, value, save, self.dialect()),
            "write dsp",
        )
    }

    #[allow(dead_code)]
    pub fn write_dsp_u32(&self, addr: u16, value: u32, save: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(&commands::write_dsp_u32(addr, value, save), "write dsp u32")
    }

    #[allow(dead_code)]
    pub fn write_dsp_delay(&self, addr: u16, samples: u32, save: bool) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp_delay(addr, samples, save, self.dialect()),
            "write dsp delay",
        )
    }

    #[allow(dead_code)]
    pub fn write_dsp_selector_u32(&self, addr: u16, value: u32) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp_u32_le_with_ctrl(addr, value, 0x80),
            "write dsp selector",
        )
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
    pub fn set_routing(
        &self,
        input: u8,
        output: u8,
        enabled: bool,
        gain_db: f32,
    ) -> Result<(), UsbHidError> {
        let table = self
            .profile
            .dsp_table
            .ok_or_else(|| UsbHidError::Protocol("routing unsupported".into()))?;
        let routing = table.routing;
        if input >= routing.rows || output >= routing.cols {
            return Err(UsbHidError::Protocol(format!(
                "routing index ({input}, {output}) out of range (max {}x{})",
                routing.rows, routing.cols
            )));
        }

        let flat_idx = input as u16 * routing.cols as u16 + output as u16;
        let status_addr = routing.status_base + flat_idx;
        let gain_addr = routing.gain_base + flat_idx;

        self.set_channel_gain(gain_addr, gain_db)?;
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                // Mixer status: 2=enabled, 1=disabled (matches minidsp-rs WriteInt).
                // Uses 0x13 with LE u32 value — audio-verified on 2x4 HD.
                let val = if enabled { 2u32 } else { 1u32 };
                self.write_dsp_selector_u32(status_addr, val)
            }
            ValueFormat::FixedPoint => {
                self.write_dsp(status_addr, if enabled { 1.0 } else { 0.0 }, true)
            }
        }
    }

    #[allow(dead_code)]
    pub fn set_channel_mute(&self, addr: u16, muted: bool) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                // Mute status: 1=muted, 2=unmuted (matches minidsp-rs WriteInt).
                let val = if muted { 1u32 } else { 2u32 };
                self.write_dsp_selector_u32(addr, val)
            }
            ValueFormat::FixedPoint => {
                let val = if muted { 0.0 } else { 1.0 };
                self.write_dsp(addr, val, true)
            }
        }
    }

    #[allow(dead_code)]
    pub fn write_dsp_linked_u32(
        &self,
        addr: u16,
        value: u32,
        linked_addrs: &[u16],
    ) -> Result<(), UsbHidError> {
        self.expect_write_ok(
            &commands::write_dsp_linked_u32(addr, value, linked_addrs),
            "write dsp linked u32",
        )
    }

    #[allow(dead_code)]
    pub fn set_channel_delay_samples(&self, addr: u16, samples: u32) -> Result<(), UsbHidError> {
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => self.write_dsp_delay(addr, samples, false),
            ValueFormat::FixedPoint => self.write_dsp_delay(addr, samples, true),
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

    /// Execute a raw command and mirror successful write side-effects into
    /// the PEQ shadow so `/config` reflects TCP bridge / WebSocket activity.
    pub fn raw_command(&self, payload: &[u8]) -> Result<Vec<u8>, UsbHidError> {
        let mut resp = self.bus.command(payload)?;
        // Trim HID echo garbage: for write commands, real DSP100/2x4 HD hardware
        // returns an unframed 0x01-prefixed echo of the full 64-byte HID buffer.
        // decode_hid_response strips the 0x01 prefix but returns all 63 trailing
        // bytes including garbage. Trim to the original request length so TCP
        // bridge clients (Device Console, minidsp-rs) only see the echoed command.
        if let Some(opcode) = payload.first().copied() {
            if commands::is_write_command(opcode)
                && resp.first().copied() == payload.first().copied()
                && resp.len() > payload.len()
            {
                resp.truncate(payload.len());
            }
        }
        let parsed = commands::parse_response_for_request(payload, &resp, self.dialect());
        if matches!(
            parsed,
            Response::Ack | Response::PresetComplete | Response::ResetComplete
        ) {
            self.mirror_raw_to_shadow(payload);
        }
        Ok(resp)
    }

    /// Parse a raw write payload and update the PEQ shadow accordingly.
    fn mirror_raw_to_shadow(&self, payload: &[u8]) {
        // Biquad write: CMD_WRITE_BIQUAD (0x13), ctrl=0x80, addr[2], pad[2], coeffs[20]
        if payload.first().copied() == Some(commands::CMD_WRITE_BIQUAD)
            && payload.len() >= 26
            && payload[1] == 0x80
        {
            let addr = u16::from_be_bytes([payload[2], payload[3]]);
            let mut coeffs = [0.0f32; 5];
            let mut offset = 6;
            for coeff in &mut coeffs {
                if let Some(bytes) = payload.get(offset..offset + 4) {
                    let arr: [u8; 4] = bytes.try_into().unwrap();
                    *coeff = minidsp_protocol::dialect::decode_value(&arr, self.dialect());
                    offset += 4;
                }
            }
            if let Ok(mut shadow) = self.peq_shadow.lock() {
                if let Some(entry) = shadow.get_mut(&addr) {
                    entry.coeffs = coeffs;
                }
            }
            return;
        }

        // Bypass filter: CMD_BYPASS_FILTER (0x13 with ctrl bit pattern)
        if payload.first().copied() == Some(commands::CMD_BYPASS_FILTER) && payload.len() >= 6 {
            let addr = u16::from_be_bytes([payload[2], payload[3]]);
            let bypass = payload[1] & 0x80 != 0;
            if let Ok(mut shadow) = self.peq_shadow.lock() {
                if let Some(entry) = shadow.get_mut(&addr) {
                    entry.bypass = bypass;
                }
            }
        }
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
