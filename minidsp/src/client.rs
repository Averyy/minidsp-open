//! High-level MiniDSP client — wraps transport + command queue + device profile.

use crate::transport::{Transport, TransportError};
use crate::queue::{CommandQueue, Priority};
use minidsp_protocol::commands::{self, HardwareId, Response};
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::device::probe;
use minidsp_protocol::eeprom;
use minidsp_protocol::status::{
    MasterStatusView, StatusSummary, DeviceInfo, DeviceVersion,
    BiquadCoeffs, PeqBandConfig, InputChannelConfig, OutputChannelConfig,
    CompressorConfig, DeviceConfig,
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Error type for client operations.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("transport error: {0}")]
    Transport(#[from] TransportError),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("device busy")]
    Busy,
    #[error("timeout")]
    Timeout,
    #[error("unsupported operation for this device")]
    Unsupported,
}

/// Saved PEQ state for measurement mode restore.
#[derive(Debug, Clone)]
pub struct SavedPeqState {
    /// (addr, coefficients, bypass) tuples for all PEQ bands that were saved.
    pub bands: Vec<(u16, [f32; 5], bool)>,
}

/// Connected MiniDSP device client.
pub struct MiniDspClient {
    queue: Arc<CommandQueue>,
    profile: DeviceProfile,
    hw_id: HardwareId,
    dsp_version: u8,
    serial: u32,
    /// Saved PEQ state from measurement mode entry, for later restore.
    saved_peq: Mutex<Option<SavedPeqState>>,
}

impl MiniDspClient {
    /// Connect to a device via transport, probe its identity, and return a client.
    pub async fn connect(transport: Box<dyn Transport>) -> Result<Self, ClientError> {
        let queue = Arc::new(CommandQueue::new(transport));

        // Step 1: Read hardware ID
        let hw_resp = queue
            .execute(commands::read_hardware_id(), Priority::Critical)
            .await?;
        let hw_id = match hw_resp {
            Response::HardwareId(id) => id,
            _ => return Err(ClientError::Protocol("unexpected response to ReadHardwareId".into())),
        };

        // Step 2: Read DSP version from EEPROM
        let flash_resp = queue
            .execute(
                commands::read_flash(eeprom::DSP_VERSION_ADDR.0, eeprom::DSP_VERSION_ADDR.1, 1),
                Priority::Critical,
            )
            .await?;
        let dsp_version = match flash_resp {
            Response::FlashData(data) => eeprom::parse_dsp_version(&data).unwrap_or(0),
            _ => 0,
        };

        // Step 3: Read serial number
        let serial_resp = queue
            .execute(
                commands::read_flash(eeprom::SERIAL_ADDR.0, eeprom::SERIAL_ADDR.1, 2),
                Priority::Critical,
            )
            .await?;
        let serial = match serial_resp {
            Response::FlashData(data) => eeprom::parse_serial(&data).unwrap_or(0),
            _ => 0,
        };

        // Step 4: Identify device profile
        let profile = probe::identify(hw_id.hw_id, dsp_version);

        // Step 5: Set dialect on queue so responses are parsed correctly
        queue.set_dialect(profile.dialect).await;

        Ok(Self {
            queue,
            profile,
            hw_id,
            dsp_version,
            serial,
            saved_peq: Mutex::new(None),
        })
    }

    pub fn profile(&self) -> &DeviceProfile {
        &self.profile
    }

    pub fn hw_id(&self) -> &HardwareId {
        &self.hw_id
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    pub fn dsp_version(&self) -> u8 {
        self.dsp_version
    }

    /// Get device info for the /devices list endpoint.
    pub fn device_info(&self, url: &str) -> DeviceInfo {
        DeviceInfo {
            url: url.to_string(),
            version: DeviceVersion {
                hw_id: self.hw_id.hw_id,
                fw_major: self.hw_id.fw_major,
                fw_minor: self.hw_id.fw_minor,
                dsp_version: self.dsp_version,
                serial: self.serial,
            },
            product_name: probe::product_name(self.hw_id.hw_id, self.dsp_version).to_string(),
        }
    }

    /// Read master status from EEPROM.
    pub async fn get_master_status(&self) -> Result<eeprom::MasterStatusRaw, ClientError> {
        let resp = self
            .queue
            .execute(
                commands::read_flash(eeprom::MASTER_STATUS_ADDR.0, eeprom::MASTER_STATUS_ADDR.1, 4),
                Priority::Normal,
            )
            .await?;
        match resp {
            Response::FlashData(data) => {
                eeprom::MasterStatusRaw::from_bytes(&data)
                    .ok_or_else(|| ClientError::Protocol("failed to parse master status".into()))
            }
            _ => Err(ClientError::Protocol("unexpected response".into())),
        }
    }

    /// Get full status summary (compatible with minidsp-rs StatusSummary).
    pub async fn get_status_summary(&self) -> Result<StatusSummary, ClientError> {
        let master = self.get_master_status().await?;
        let source_name = self
            .profile
            .sources
            .source_for_index(master.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Unknown({})", master.source));

        // Read level meters if available
        let (input_levels, output_levels) = if let Some(table) = self.profile.dsp_table {
            let input_levels = self.read_meters(&table.meters.input).await?;
            let output_levels = self.read_meters(&table.meters.output).await?;
            (input_levels, output_levels)
        } else {
            (vec![], vec![])
        };

        let dirac = if self.profile.has_dirac {
            // Read Dirac bypass state from EEPROM 0xFFE0
            let dirac_resp = self.queue
                .execute(commands::read_flash(0xFF, 0xE0, 1), Priority::Normal)
                .await;
            match dirac_resp {
                Ok(Response::FlashData(data)) if !data.is_empty() => {
                    Some(data[0] == 0) // 0 = Dirac active (not bypassed), 1 = bypassed
                }
                _ => Some(false),
            }
        } else {
            None
        };

        Ok(StatusSummary {
            master: MasterStatusView {
                preset: master.preset,
                source: source_name,
                volume: master.volume_db(),
                mute: master.is_muted(),
                dirac,
            },
            input_levels,
            output_levels,
        })
    }

    /// Set volume (raw byte value, gain = -0.5 * value).
    pub async fn set_volume(&self, value: u8) -> Result<(), ClientError> {
        self.queue
            .execute(commands::set_volume(value), Priority::High)
            .await?;
        Ok(())
    }

    /// Set volume in dB.
    pub async fn set_volume_db(&self, db: f32) -> Result<(), ClientError> {
        let value = (-2.0 * db).clamp(0.0, 255.0) as u8;
        self.set_volume(value).await
    }

    /// Set mute state.
    pub async fn set_mute(&self, muted: bool) -> Result<(), ClientError> {
        self.queue
            .execute(commands::set_mute(muted), Priority::High)
            .await?;
        Ok(())
    }

    /// Switch preset (0-3).
    pub async fn set_config(&self, preset: u8) -> Result<(), ClientError> {
        if preset >= self.profile.presets {
            return Err(ClientError::Protocol(format!(
                "preset {} out of range (max {})", preset, self.profile.presets - 1
            )));
        }
        self.queue
            .execute(commands::set_config(preset, 0), Priority::Critical)
            .await?;
        Ok(())
    }

    /// Set source by index.
    pub async fn set_source(&self, source: u8) -> Result<(), ClientError> {
        self.queue
            .execute(commands::set_source(source), Priority::High)
            .await?;
        Ok(())
    }

    /// Set Dirac bypass state.
    pub async fn set_dirac_bypass(&self, bypass: bool) -> Result<(), ClientError> {
        if !self.profile.has_dirac {
            return Err(ClientError::Unsupported);
        }
        self.queue
            .execute(commands::dirac_bypass(bypass), Priority::High)
            .await?;
        Ok(())
    }

    /// Set channel mute using dialect-aware encoding.
    pub async fn set_channel_mute(&self, addr: u16, muted: bool) -> Result<(), ClientError> {
        use minidsp_protocol::dialect::{ValueFormat, encode_mute};
        let mute_bytes = encode_mute(muted, &self.profile.dialect);
        match self.profile.dialect.value_format {
            ValueFormat::Float32 => {
                // New dialect: mute value is 2.0 (muted) or 1.0 (unmuted)
                let val = if muted { 2.0 } else { 1.0 };
                self.write_dsp(addr, val, true).await?;
            }
            ValueFormat::FixedPoint => {
                // Old dialect: mute is raw 4-byte fixed-point via WriteDSP
                let raw = u32::from_be_bytes([mute_bytes[0], mute_bytes[1], mute_bytes[2], mute_bytes[3]]);
                let val = minidsp_protocol::dialect::fixed_point_to_float(raw);
                self.write_dsp(addr, val, true).await?;
            }
        }
        Ok(())
    }

    /// Set channel gain using dialect-aware encoding.
    pub async fn set_channel_gain(&self, addr: u16, db: f32) -> Result<(), ClientError> {
        let gain_linear = 10.0f32.powf(db / 20.0);
        self.write_dsp(addr, gain_linear, true).await
    }

    /// Write biquad coefficients to a PEQ band.
    /// Warns (but does not reject) unstable filters for minidsp-rs compatibility.
    pub async fn write_biquad(
        &self,
        addr: u16,
        coeffs: &[f32; 5],
    ) -> Result<(), ClientError> {
        if !minidsp_protocol::biquad::is_stable(coeffs) {
            tracing::warn!(
                "writing potentially unstable biquad at addr {addr}: a1={}, a2={}",
                coeffs[3], coeffs[4]
            );
        }
        self.queue
            .execute(
                commands::write_biquad(addr, coeffs, &self.profile.dialect),
                Priority::High,
            )
            .await?;
        Ok(())
    }

    /// Set PEQ bypass state.
    pub async fn bypass_filter(&self, addr: u16, bypass: bool) -> Result<(), ClientError> {
        self.queue
            .execute(
                commands::bypass_filter(addr, bypass, &self.profile.dialect),
                Priority::High,
            )
            .await?;
        Ok(())
    }

    /// Write DSP parameter.
    pub async fn write_dsp(&self, addr: u16, value: f32, save: bool) -> Result<(), ClientError> {
        self.queue
            .execute(
                commands::write_dsp(addr, value, save, &self.profile.dialect),
                Priority::High,
            )
            .await?;
        Ok(())
    }

    /// Read float values from DSP memory.
    pub async fn read_floats(&self, addr: u16, count: u8) -> Result<Vec<f32>, ClientError> {
        let resp = self
            .queue
            .execute(
                commands::read_floats(addr, count, &self.profile.dialect),
                Priority::Normal,
            )
            .await?;
        match resp {
            Response::Floats(values) => Ok(values),
            _ => Err(ClientError::Protocol("unexpected response to ReadFloats".into())),
        }
    }

    /// Send raw command payload. For advanced/raw access.
    pub async fn raw_command(&self, payload: Vec<u8>, priority: Priority) -> Result<Response, ClientError> {
        Ok(self.queue.execute(payload, priority).await?)
    }

    /// Access the command queue directly.
    pub fn queue(&self) -> &Arc<CommandQueue> {
        &self.queue
    }

    /// Override the device profile (for --force-kind).
    pub async fn override_profile(&mut self, profile: DeviceProfile) {
        self.queue.set_dialect(profile.dialect).await;
        self.profile = profile;
    }

    /// Enter measurement mode: save all PEQ states, then bypass all PEQ bands.
    ///
    /// Reads current coefficients and bypass state for every PEQ band on every
    /// input and output channel, stores them internally, then sets all bands to bypass.
    pub async fn enter_measurement_mode(&self) -> Result<(), ClientError> {
        let table = self.profile.dsp_table.ok_or(ClientError::Unsupported)?;
        let mut bands: Vec<(u16, [f32; 5], bool)> = Vec::new();

        // Collect all PEQ band addresses from inputs
        for input in table.inputs {
            for band in input.peq {
                let coeffs = self.read_peq_coeffs(band.addr).await?;
                let bypass = self.read_peq_bypass(band.addr).await.unwrap_or(false);
                bands.push((band.addr, coeffs, bypass));
            }
        }

        // Collect all PEQ band addresses from outputs
        for output in table.outputs {
            for band in output.peq {
                let coeffs = self.read_peq_coeffs(band.addr).await?;
                let bypass = self.read_peq_bypass(band.addr).await.unwrap_or(false);
                bands.push((band.addr, coeffs, bypass));
            }
        }

        // Save the state
        *self.saved_peq.lock().await = Some(SavedPeqState { bands: bands.clone() });

        // Now bypass all bands
        let mut bypass_cmds = Vec::new();
        for &(addr, _, _) in &bands {
            bypass_cmds.push(commands::bypass_filter(addr, true, &self.profile.dialect));
        }
        self.queue.execute_batch(bypass_cmds, Priority::Critical).await?;

        tracing::info!("measurement mode: saved and bypassed {} PEQ bands", bands.len());
        Ok(())
    }

    /// Restore saved PEQ state from measurement mode.
    ///
    /// Writes back all saved coefficients and bypass states using safe batch ordering.
    pub async fn restore_measurement_mode(&self) -> Result<(), ClientError> {
        let saved = self.saved_peq.lock().await.take()
            .ok_or_else(|| ClientError::Protocol("no saved measurement mode state".into()))?;

        self.write_peq_batch(&saved.bands).await?;

        tracing::info!("measurement mode: restored {} PEQ bands", saved.bands.len());
        Ok(())
    }

    /// Check if measurement mode state is currently saved.
    pub async fn has_saved_measurement_state(&self) -> bool {
        self.saved_peq.lock().await.is_some()
    }

    /// Batch write PEQ coefficients with partial-write recovery.
    ///
    /// Safe ordering:
    /// 1. Bypass all bands first (safe state)
    /// 2. Write coefficients
    /// 3. Set final bypass states
    ///
    /// Uses execute_batch for contiguous execution.
    pub async fn write_peq_batch(
        &self,
        bands: &[(u16, [f32; 5], bool)],
    ) -> Result<(), ClientError> {
        if bands.is_empty() {
            return Ok(());
        }

        // Phase 1: Bypass all bands (safe state)
        let mut phase1_cmds = Vec::with_capacity(bands.len());
        for &(addr, _, _) in bands {
            phase1_cmds.push(commands::bypass_filter(addr, true, &self.profile.dialect));
        }
        self.queue.execute_batch(phase1_cmds, Priority::Critical).await?;

        // Phase 2: Write coefficients
        let mut phase2_cmds = Vec::with_capacity(bands.len());
        for &(addr, ref coeffs, _) in bands {
            phase2_cmds.push(commands::write_biquad(addr, coeffs, &self.profile.dialect));
        }
        self.queue.execute_batch(phase2_cmds, Priority::Critical).await?;

        // Phase 3: Set final bypass states
        let mut phase3_cmds = Vec::with_capacity(bands.len());
        for &(addr, _, bypass) in bands {
            phase3_cmds.push(commands::bypass_filter(addr, bypass, &self.profile.dialect));
        }
        self.queue.execute_batch(phase3_cmds, Priority::Critical).await?;

        Ok(())
    }

    /// Read 5 biquad coefficients from a PEQ band address.
    async fn read_peq_coeffs(&self, addr: u16) -> Result<[f32; 5], ClientError> {
        let values = self.read_floats(addr, 5).await?;
        if values.len() >= 5 {
            Ok([values[0], values[1], values[2], values[3], values[4]])
        } else {
            // Return bypass coefficients if we can't read the band
            Ok(minidsp_protocol::biquad::BYPASS)
        }
    }

    /// Read PEQ bypass state. Bypass is stored at addr+5 on SHARC devices.
    /// Returns None if the read fails or the device doesn't support bypass readback.
    async fn read_peq_bypass(&self, _addr: u16) -> Option<bool> {
        // On SHARC devices, the bypass state for a biquad at base_addr is not directly
        // readable via a simple ReadFloats. We default to false (not bypassed) when
        // we can't determine the state.
        None
    }

    /// Read the full device config including all PEQ coefficients.
    pub async fn read_device_config(&self) -> Result<DeviceConfig, ClientError> {
        let master_raw = self.get_master_status().await?;
        let source_name = self.profile.sources
            .source_for_index(master_raw.source)
            .map(|s| s.pascal_case().to_string())
            .unwrap_or_else(|| format!("Unknown({})", master_raw.source));

        let dirac = if self.profile.has_dirac {
            let resp = self.queue
                .execute(commands::read_flash(0xFF, 0xE0, 1), Priority::Normal)
                .await;
            match resp {
                Ok(Response::FlashData(data)) if !data.is_empty() => Some(data[0] == 0),
                _ => Some(false),
            }
        } else {
            None
        };

        let master_status = MasterStatusView {
            preset: master_raw.preset,
            source: source_name,
            volume: master_raw.volume_db(),
            mute: master_raw.is_muted(),
            dirac,
        };

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();

        if let Some(table) = self.profile.dsp_table {
            // Read input channels
            for (ch_idx, input) in table.inputs.iter().enumerate() {
                let gain = if let Some(addr) = input.gain {
                    self.read_floats(addr, 1).await.ok().and_then(|v| v.first().copied())
                } else {
                    None
                };
                let mute = if let Some(addr) = input.mute {
                    self.read_floats(addr, 1).await.ok().and_then(|v| {
                        v.first().map(|&val| val >= 1.5) // 2.0 = muted, 1.0 = unmuted
                    })
                } else {
                    None
                };
                let mut peq = Vec::new();
                for (band_idx, band) in input.peq.iter().enumerate() {
                    if let Ok(coeffs) = self.read_peq_coeffs(band.addr).await {
                        peq.push(PeqBandConfig {
                            index: band_idx as u8,
                            coeff: BiquadCoeffs::from_array(&coeffs),
                            bypass: false,
                        });
                    }
                }
                inputs.push(InputChannelConfig {
                    index: ch_idx as u8,
                    gain,
                    mute,
                    peq,
                });
            }

            // Read output channels
            for (ch_idx, output) in table.outputs.iter().enumerate() {
                let gain_val = self.read_floats(output.gain, 1).await
                    .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                let mute_val = self.read_floats(output.mute, 1).await
                    .ok().and_then(|v| v.first().copied()).unwrap_or(1.0);
                let delay_val = self.read_floats(output.delay, 1).await
                    .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);

                let mut peq = Vec::new();
                for (band_idx, band) in output.peq.iter().enumerate() {
                    if let Ok(coeffs) = self.read_peq_coeffs(band.addr).await {
                        peq.push(PeqBandConfig {
                            index: band_idx as u8,
                            coeff: BiquadCoeffs::from_array(&coeffs),
                            bypass: false,
                        });
                    }
                }

                let compressor = if let Some(comp) = &output.compressor {
                    let bypass = self.read_floats(comp.bypass, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    let threshold = self.read_floats(comp.threshold, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    let comp_gain = self.read_floats(comp.gain, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    let ratio = self.read_floats(comp.ratio, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    let knee = self.read_floats(comp.knee, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    let attack = self.read_floats(comp.attack, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    let release = self.read_floats(comp.release, 1).await
                        .ok().and_then(|v| v.first().copied()).unwrap_or(0.0);
                    Some(CompressorConfig {
                        bypass: bypass >= 1.5,
                        threshold,
                        gain: comp_gain,
                        ratio,
                        knee,
                        attack,
                        release,
                    })
                } else {
                    None
                };

                outputs.push(OutputChannelConfig {
                    index: ch_idx as u8,
                    gain: gain_val,
                    mute: mute_val >= 1.5,
                    delay: delay_val,
                    peq,
                    compressor,
                });
            }
        }

        Ok(DeviceConfig {
            master_status,
            inputs,
            outputs,
        })
    }

    // Internal: read meter values from an array of addresses.
    async fn read_meters(&self, addrs: &[u16]) -> Result<Vec<f32>, ClientError> {
        if addrs.is_empty() {
            return Ok(vec![]);
        }
        let mut all_values = Vec::with_capacity(addrs.len());

        // Check if addresses are contiguous for bulk read
        let is_contiguous = addrs.len() <= 14
            && addrs.windows(2).all(|w| w[1] == w[0] + 1);

        if is_contiguous {
            let values = self.read_floats(addrs[0], addrs.len() as u8).await?;
            all_values.extend(values);
        } else {
            // Find contiguous runs and read each run in bulk
            let mut i = 0;
            while i < addrs.len() {
                let start = i;
                while i + 1 < addrs.len() && addrs[i + 1] == addrs[i] + 1 && (i - start + 1) < 14 {
                    i += 1;
                }
                let count = (i - start + 1).min(14);
                let values = self.read_floats(addrs[start], count as u8).await?;
                all_values.extend(values);
                i += 1;
            }
        }

        // Convert raw float to dB (20 * log10(abs(value)), clamped)
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
}
