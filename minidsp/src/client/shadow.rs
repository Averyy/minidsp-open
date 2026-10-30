use super::*;
use minidsp_protocol::shadow::{
    parse_raw_bypass_filter, parse_raw_linked_selector_write, parse_raw_selector_write,
    parse_raw_write_biquad,
};

type CommandBatch = Vec<Vec<u8>>;
type MeasurementRestoreBatches = (CommandBatch, CommandBatch, CommandBatch);

pub(super) async fn execute_peq_batch(
    queue: &Arc<CommandQueue>,
    dialect: &minidsp_protocol::dialect::Dialect,
    peq_shadow: &Arc<Mutex<HashMap<u16, PeqShadow>>>,
    bands: &[(u16, [f32; 5], bool)],
) -> Result<(), ClientError> {
    let mut phase1_cmds = Vec::with_capacity(bands.len());
    for &(addr, _, _) in bands {
        phase1_cmds.push(commands::bypass_filter(addr, true, dialect));
    }
    ensure_write_batch(
        queue.execute_batch(phase1_cmds, Priority::Critical).await?,
        "set peq bypass before batch write",
    )?;

    let mut phase2_cmds = Vec::with_capacity(bands.len());
    for &(addr, ref coeffs, _) in bands {
        phase2_cmds.push(commands::write_biquad(addr, coeffs, dialect));
    }
    ensure_write_batch(
        queue.execute_batch(phase2_cmds, Priority::Critical).await?,
        "write peq batch coefficients",
    )?;

    let mut phase3_cmds = Vec::with_capacity(bands.len());
    for &(addr, _, bypass) in bands {
        phase3_cmds.push(commands::bypass_filter(addr, bypass, dialect));
    }
    ensure_write_batch(
        queue.execute_batch(phase3_cmds, Priority::Critical).await?,
        "restore peq bypass after batch write",
    )?;

    let mut shadow = peq_shadow.lock().await;
    for &(addr, coeffs, bypass) in bands {
        shadow.insert(addr, PeqShadow { coeffs, bypass });
    }

    Ok(())
}

pub(super) fn build_measurement_restore_batches(
    current_shadow: &HashMap<u16, PeqShadow>,
    saved: &[(u16, [f32; 5], bool)],
    dialect: &minidsp_protocol::dialect::Dialect,
) -> MeasurementRestoreBatches {
    let mut phase1_cmds = Vec::new();
    let mut phase2_cmds = Vec::new();
    let mut phase3_cmds = Vec::new();

    for &(addr, coeffs, bypass) in saved {
        let current = current_shadow.get(&addr).copied();
        let coeffs_changed = current
            .map(|shadow| shadow.coeffs != coeffs)
            .unwrap_or(true);
        let bypass_changed = current
            .map(|shadow| shadow.bypass != bypass)
            .unwrap_or(true);

        if coeffs_changed {
            if !current.map(|shadow| shadow.bypass).unwrap_or(false) {
                phase1_cmds.push(commands::bypass_filter(addr, true, dialect));
            }
            phase2_cmds.push(commands::write_biquad(addr, &coeffs, dialect));
        }

        if coeffs_changed || bypass_changed {
            phase3_cmds.push(commands::bypass_filter(addr, bypass, dialect));
        }
    }

    (phase1_cmds, phase2_cmds, phase3_cmds)
}

pub(super) fn ensure_write_batch(
    responses: Vec<Response>,
    context: &str,
) -> Result<(), ClientError> {
    for resp in responses {
        MiniDspClient::ensure_write_response(resp, context)?;
    }
    Ok(())
}

pub(super) async fn apply_peq_shadow_bypass(
    peq_shadow: &Arc<Mutex<HashMap<u16, PeqShadow>>>,
    bands: &[(u16, [f32; 5], bool)],
    bypass: bool,
) {
    let mut shadow = peq_shadow.lock().await;
    for &(addr, _, _) in bands {
        if let Some(entry) = shadow.get_mut(&addr) {
            entry.bypass = bypass;
        }
    }
}

fn merge_trusted_snapshot(
    mut existing: PresetShadowState,
    captured: PresetShadowState,
) -> PresetShadowState {
    existing.peq.extend(captured.peq);
    existing.selector.extend(captured.selector);
    existing.scalar.extend(captured.scalar);
    existing.fir_blocks.extend(captured.fir_blocks);
    existing
}

pub(super) async fn mirror_raw_command_to_shadow<F>(
    payload: &[u8],
    dialect: &minidsp_protocol::dialect::Dialect,
    peq_shadow: &Arc<Mutex<HashMap<u16, PeqShadow>>>,
    scalar_shadow: &Arc<Mutex<HashMap<u16, u32>>>,
    selector_shadow: &Arc<Mutex<HashMap<u16, u16>>>,
    prefer_scalar_shadow_for_addr: F,
) where
    F: Fn(u16) -> bool,
{
    if let Some((addr, coeffs)) = parse_raw_write_biquad(payload, dialect) {
        let mut shadow = peq_shadow.lock().await;
        if let Some(entry) = shadow.get_mut(&addr) {
            entry.coeffs = coeffs;
        }
        return;
    }

    if let Some((addr, bypass)) = parse_raw_bypass_filter(payload) {
        let mut shadow = peq_shadow.lock().await;
        if let Some(entry) = shadow.get_mut(&addr) {
            entry.bypass = bypass;
        }
        return;
    }

    if let Some((addr, value)) = parse_raw_selector_write(payload, dialect)
        .or_else(|| parse_raw_linked_selector_write(payload, dialect))
    {
        if prefer_scalar_shadow_for_addr(addr) {
            scalar_shadow.lock().await.insert(addr, value as u32);
        } else {
            selector_shadow.lock().await.insert(addr, value);
        }
        return;
    }

    if dialect.value_format == ValueFormat::Float32
        && payload.first().copied() == Some(commands::CMD_WRITE_DSP)
        && payload.len() >= 8
    {
        let addr = u16::from_be_bytes([payload[2], payload[3]]);
        let raw = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
        scalar_shadow.lock().await.insert(addr, raw);
    }
}

impl MiniDspClient {
    async fn capture_active_shadow(&self) -> PresetShadowState {
        let peq = self
            .peq_shadow
            .lock()
            .await
            .iter()
            .map(|(&addr, entry)| {
                (
                    addr,
                    CachedPeqShadow {
                        coeffs: entry.coeffs,
                        bypass: entry.bypass,
                    },
                )
            })
            .collect();
        let selector = self.selector_shadow.lock().await.clone();
        let scalar = self.scalar_shadow.lock().await.clone();
        let fir_blocks = self.fir_shadow.lock().await.clone();
        PresetShadowState {
            peq,
            selector,
            scalar,
            fir_blocks,
        }
    }

    async fn apply_active_shadow_snapshot(&self, snapshot: Option<PresetShadowState>) {
        {
            let mut peq_shadow = self.peq_shadow.lock().await;
            *peq_shadow = build_peq_shadow(&self.profile);
            if let Some(snapshot) = &snapshot {
                for (&addr, entry) in &snapshot.peq {
                    peq_shadow.insert(
                        addr,
                        PeqShadow {
                            coeffs: entry.coeffs,
                            bypass: entry.bypass,
                        },
                    );
                }
            }
        }
        {
            let mut selector_shadow = self.selector_shadow.lock().await;
            selector_shadow.clear();
            if let Some(snapshot) = &snapshot {
                selector_shadow.extend(
                    snapshot
                        .selector
                        .iter()
                        .map(|(&addr, &value)| (addr, value)),
                );
            }
        }
        {
            let mut scalar_shadow = self.scalar_shadow.lock().await;
            scalar_shadow.clear();
            if let Some(snapshot) = &snapshot {
                scalar_shadow.extend(snapshot.scalar.iter().map(|(&addr, &value)| (addr, value)));
            }
        }
        {
            let mut fir_shadow = self.fir_shadow.lock().await;
            fir_shadow.clear();
            if let Some(snapshot) = &snapshot {
                fir_shadow.extend(
                    snapshot
                        .fir_blocks
                        .iter()
                        .map(|(&addr, coeffs)| (addr, coeffs.clone())),
                );
            }
        }
    }

    pub(super) async fn update_active_preset(&self, preset: u8) {
        let previous = {
            let mut active_preset = self.active_preset.lock().await;
            if *active_preset == Some(preset) {
                return;
            }
            let previous = *active_preset;
            *active_preset = Some(preset);
            previous
        };

        if let Some(previous) = previous {
            let skip_capture = {
                self.skip_trusted_shadow_capture_once
                    .lock()
                    .await
                    .remove(&previous)
            };
            if !skip_capture {
                let snapshot = self.capture_active_shadow().await;
                let trusted = self
                    .trusted_snapshot_presets
                    .lock()
                    .await
                    .contains(&previous);
                let snapshot = if trusted {
                    self.preset_shadows
                        .lock()
                        .await
                        .get(&previous)
                        .cloned()
                        .map(|existing| merge_trusted_snapshot(existing, snapshot.clone()))
                        .unwrap_or(snapshot)
                } else {
                    snapshot
                };
                self.preset_shadows.lock().await.insert(previous, snapshot);
            }
        }

        let snapshot = self.preset_shadows.lock().await.get(&preset).cloned();
        self.apply_active_shadow_snapshot(snapshot).await;
    }

    pub(crate) async fn store_preset_shadow_snapshot(
        &self,
        preset: u8,
        snapshot: PresetShadowState,
    ) {
        self.preset_shadows
            .lock()
            .await
            .insert(preset, snapshot.clone());
        if *self.active_preset.lock().await == Some(preset) {
            self.apply_active_shadow_snapshot(Some(snapshot)).await;
        }
    }

    pub(super) async fn set_preset_token(&self, preset: u8, token: [u8; 4]) {
        self.preset_tokens.lock().await.insert(preset, token);
    }

    pub(crate) async fn mark_snapshot_trusted(&self, preset: u8) {
        self.trusted_snapshot_presets.lock().await.insert(preset);
        if *self.active_preset.lock().await == Some(preset) {
            self.skip_trusted_shadow_capture_once
                .lock()
                .await
                .insert(preset);
        }
    }

    pub(super) async fn note_preset_mutation(&self) {
        if let Some(preset) = *self.active_preset.lock().await {
            self.skip_trusted_shadow_capture_once
                .lock()
                .await
                .remove(&preset);
        }
    }

    /// Mirror the side effects of a successful raw-bridge write into the
    /// client's shadow state so `/config` can reflect raw WS/TCP activity.
    pub async fn mirror_raw_command_if_success(&self, payload: &[u8], raw_response: &[u8]) {
        let parsed =
            commands::parse_response_for_request(payload, raw_response, &self.profile.dialect);
        if matches!(
            parsed,
            Response::Ack | Response::PresetComplete | Response::ResetComplete
        ) {
            if matches!(
                payload.first(),
                Some(&commands::CMD_WRITE_DSP)
                    | Some(&commands::CMD_WRITE_BIQUAD)
                    | Some(&commands::CMD_BYPASS_FILTER)
            ) {
                self.note_preset_mutation().await;
            }
            mirror_raw_command_to_shadow(
                payload,
                &self.profile.dialect,
                &self.peq_shadow,
                &self.scalar_shadow,
                &self.selector_shadow,
                |addr| self.prefers_scalar_shadow_for_addr(addr),
            )
            .await;
        }
    }

    pub async fn export_shadow_state(&self) -> DeviceShadowState {
        let active_shadow = self.capture_active_shadow().await;
        let active_preset = *self.active_preset.lock().await;
        let mut presets = self.preset_shadows.lock().await.clone();
        if let Some(active_preset) = active_preset {
            presets.insert(active_preset, active_shadow.clone());
        }
        let preset_tokens = self.preset_tokens.lock().await.clone();
        let trusted_snapshot_presets = self.trusted_snapshot_presets.lock().await.clone();
        DeviceShadowState {
            presets,
            peq: active_shadow.peq,
            selector: active_shadow.selector,
            scalar: active_shadow.scalar,
            preset_tokens,
            trusted_snapshot_presets,
        }
    }

    pub async fn apply_shadow_state(&self, state: &DeviceShadowState) {
        let active_preset = *self.active_preset.lock().await;
        let legacy_shadow = (!state.peq.is_empty()
            || !state.selector.is_empty()
            || !state.scalar.is_empty())
        .then(|| PresetShadowState {
            peq: state.peq.clone(),
            selector: state.selector.clone(),
            scalar: state.scalar.clone(),
            fir_blocks: HashMap::new(),
        });

        {
            let mut preset_shadows = self.preset_shadows.lock().await;
            *preset_shadows = state.presets.clone();
            if preset_shadows.is_empty()
                && let Some(active_preset) = active_preset
                && let Some(legacy_shadow) = legacy_shadow.clone()
            {
                preset_shadows.insert(active_preset, legacy_shadow);
            }
        }
        {
            let mut preset_tokens = self.preset_tokens.lock().await;
            *preset_tokens = state.preset_tokens.clone();
        }
        {
            let mut trusted_snapshot_presets = self.trusted_snapshot_presets.lock().await;
            *trusted_snapshot_presets = state.trusted_snapshot_presets.clone();
        }
        {
            self.skip_trusted_shadow_capture_once.lock().await.clear();
        }

        let snapshot = if let Some(active_preset) = active_preset {
            self.preset_shadows
                .lock()
                .await
                .get(&active_preset)
                .cloned()
                .or(legacy_shadow)
        } else {
            legacy_shadow
        };
        self.apply_active_shadow_snapshot(snapshot).await;
    }

    pub(super) fn prefers_scalar_shadow_for_addr(&self, addr: u16) -> bool {
        if !self.profile.has_unreliable_selector_readback {
            return false;
        }
        let Some(table) = self.profile.dsp_table else {
            return false;
        };
        if table
            .inputs
            .iter()
            .filter_map(|input| input.gain)
            .any(|gain| gain == addr)
        {
            return true;
        }

        if table.outputs.iter().any(|output| {
            output.gain == addr
                || output.delay == Some(addr)
                || output.compressor.is_some_and(|comp| {
                    [
                        comp.threshold,
                        comp.gain,
                        comp.ratio,
                        comp.knee,
                        comp.attack,
                        comp.release,
                    ]
                    .contains(&addr)
                })
        }) {
            return true;
        }

        let routing_span = table.routing.rows as u16 * table.routing.cols as u16;
        addr >= table.routing.gain_base && addr < table.routing.gain_base + routing_span
    }

    pub async fn refresh_modification_tokens(&self) -> Result<(), ClientError> {
        let resp = self
            .queue
            .execute(
                commands::read_flash(eeprom::TOKENS_ADDR.0, eeprom::TOKENS_ADDR.1, 16),
                Priority::Normal,
            )
            .await?;
        let Response::FlashData(data) = resp else {
            return Err(ClientError::Protocol(
                "unexpected response to read modification tokens".into(),
            ));
        };

        let mut tokens = HashMap::new();
        for (slot, chunk) in data.chunks(4).enumerate() {
            if chunk.len() == 4 {
                tokens.insert(slot as u8, [chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
        }
        let previous = self.preset_tokens.lock().await.clone();
        let changed: Vec<u8> = self
            .trusted_snapshot_presets
            .lock()
            .await
            .iter()
            .copied()
            .filter(|slot| !previous.contains_key(slot) || previous.get(slot) != tokens.get(slot))
            .collect();
        for slot in changed {
            self.trusted_snapshot_presets.lock().await.remove(&slot);
            self.preset_shadows.lock().await.remove(&slot);
            if *self.active_preset.lock().await == Some(slot) {
                self.apply_active_shadow_snapshot(None).await;
                self.skip_trusted_shadow_capture_once
                    .lock()
                    .await
                    .insert(slot);
            } else {
                self.skip_trusted_shadow_capture_once
                    .lock()
                    .await
                    .remove(&slot);
            }
        }
        *self.preset_tokens.lock().await = tokens;
        Ok(())
    }

    pub async fn active_preset_token(&self) -> Result<(u8, [u8; 4]), ClientError> {
        let preset = self.get_master_status().await?.preset;
        self.refresh_modification_tokens().await?;
        self.preset_tokens
            .lock()
            .await
            .get(&preset)
            .copied()
            .map(|token| (preset, token))
            .ok_or_else(|| {
                ClientError::Protocol(format!(
                    "missing modification token for active preset {preset}"
                ))
            })
    }

    pub async fn save_active_preset_token(&self) -> Result<(u8, [u8; 4]), ClientError> {
        let preset = self.get_master_status().await?.preset;
        let token = generate_modification_token()?;
        let addr_lo = eeprom::TOKENS_ADDR.1 + preset.saturating_mul(4);
        self.execute_write(
            commands::write_flash(eeprom::TOKENS_ADDR.0, addr_lo, &token),
            Priority::High,
            "save modification token",
        )
        .await?;
        self.preset_tokens.lock().await.insert(preset, token);
        Ok((preset, token))
    }

    pub async fn write_preset_token_exact(
        &self,
        preset: u8,
        token: [u8; 4],
    ) -> Result<(), ClientError> {
        if preset >= self.profile.presets {
            return Err(ClientError::Protocol(format!(
                "preset {} out of range (max {})",
                preset,
                self.profile.presets.saturating_sub(1)
            )));
        }
        let addr_lo = eeprom::TOKENS_ADDR.1 + preset.saturating_mul(4);
        self.execute_write(
            commands::write_flash(eeprom::TOKENS_ADDR.0, addr_lo, &token),
            Priority::High,
            "restore modification token",
        )
        .await?;
        self.preset_tokens.lock().await.insert(preset, token);
        Ok(())
    }
}
