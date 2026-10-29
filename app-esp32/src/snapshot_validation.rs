use super::DeviceSnapshot;
use minidsp_protocol::device::DeviceProfile;
use minidsp_protocol::dialect::ValueFormat;
use minidsp_protocol::source::Source;
use std::collections::HashSet;

pub fn validate_snapshot_payload(
    profile: &DeviceProfile,
    snapshot: &DeviceSnapshot,
) -> Result<(), String> {
    if snapshot.format_version != 1
        || !snapshot.completeness.strict
        || !snapshot.completeness.issues.is_empty()
    {
        return Err("unsupported or incomplete snapshot".into());
    }
    if profile.dialect.value_format != ValueFormat::Float32 {
        return Err("fixed-point snapshot restore is not supported".into());
    }
    let table = profile.dsp_table.ok_or("snapshot restore unsupported")?;
    if snapshot.device.preset_count != profile.presets
        || snapshot.presets.len() != profile.presets as usize
        || snapshot.master.active_preset >= profile.presets
        || !snapshot.master.volume.is_finite()
        || !(-127.5..=0.0).contains(&snapshot.master.volume)
        || (snapshot.master.dirac.is_some() && !profile.has_dirac)
    {
        return Err("invalid snapshot preset coverage or master state".into());
    }
    let source = Source::from_str_pascal(&snapshot.master.source)
        .or_else(|| Source::from_str_lowercase(&snapshot.master.source))
        .ok_or("unknown snapshot source")?;
    if profile.sources.index_for_source(source).is_none() {
        return Err("snapshot source unavailable on this device".into());
    }

    let mut peq = HashSet::new();
    let mut scalar = HashSet::new();
    let mut selector = HashSet::new();
    let mut polarity = HashSet::new();
    let mut delays = HashSet::new();
    let mut firs = Vec::new();
    for input in table.inputs {
        selector.extend(input.mute);
        scalar.extend(input.gain);
        polarity.extend(input.polarity);
        peq.extend(input.peq.iter().map(|band| band.addr));
        firs.extend(input.fir);
    }
    for output in table.outputs {
        selector.insert(output.mute);
        scalar.insert(output.gain);
        delays.extend(output.delay);
        polarity.extend(output.polarity);
        peq.extend(output.peq.iter().map(|band| band.addr));
        firs.extend(output.fir);
        if let Some(crossover) = output.crossover {
            for base in [crossover.lpf_addr, crossover.hpf_addr] {
                peq.extend((0..4).map(|index| base + index * 5));
            }
        }
        if let Some(comp) = output.compressor {
            selector.insert(comp.bypass);
            scalar.extend([
                comp.threshold,
                comp.gain,
                comp.ratio,
                comp.knee,
                comp.attack,
                comp.release,
            ]);
        }
    }
    for offset in 0..table.routing.rows as u16 * table.routing.cols as u16 {
        selector.insert(table.routing.status_base + offset);
        scalar.insert(table.routing.gain_base + offset);
        if let Some(base) = table.routing.polarity_base {
            polarity.insert(base + offset);
        }
    }
    selector.extend(&polarity);
    scalar.extend(&delays);
    for fir in &firs {
        selector.extend([fir.bypass, fir.taps_count]);
    }

    let mut slots = HashSet::new();
    for preset in &snapshot.presets {
        if preset.slot >= profile.presets
            || !slots.insert(preset.slot)
            || !preset.complete
            || !preset.completeness.strict
            || !preset.completeness.issues.is_empty()
        {
            return Err("duplicate, out-of-range or incomplete snapshot preset".into());
        }
        let token = preset
            .token
            .as_deref()
            .ok_or("missing modification token")?;
        if token.len() != 8 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid modification token".into());
        }
        let shadow = preset.shadow.as_ref().ok_or("missing snapshot shadow")?;
        if shadow.peq.keys().copied().collect::<HashSet<_>>() != peq
            || shadow.scalar.keys().copied().collect::<HashSet<_>>() != scalar
            || shadow.selector.keys().copied().collect::<HashSet<_>>() != selector
        {
            return Err(format!(
                "preset {} has missing or unsupported DSP addresses",
                preset.slot
            ));
        }
        if shadow
            .peq
            .values()
            .any(|entry| entry.coeffs.iter().any(|value| !value.is_finite()))
            || shadow
                .scalar
                .iter()
                .any(|(addr, raw)| !delays.contains(addr) && !f32::from_bits(*raw).is_finite())
        {
            return Err("non-finite snapshot DSP value".into());
        }
        for (addr, value) in &shadow.selector {
            let valid = if polarity.contains(addr) {
                *value <= 1
            } else if let Some(fir) = firs.iter().find(|fir| fir.taps_count == *addr) {
                *value <= fir.max_taps
            } else {
                (1..=3).contains(value)
            };
            if !valid {
                return Err(format!("invalid snapshot selector at {addr}"));
            }
        }
        let mut blocks = HashSet::new();
        for block in &preset.fir_blocks {
            let fir = firs
                .iter()
                .find(|fir| fir.data_start == block.data_start)
                .ok_or("unsupported FIR block")?;
            let taps = shadow.selector[&fir.taps_count] as usize;
            if !blocks.insert(block.data_start)
                || taps == 0
                || block.coefficients.len() != taps
                || block.coefficients.iter().any(|value| !value.is_finite())
            {
                return Err("invalid FIR coefficient block".into());
            }
        }
        for fir in &firs {
            if shadow.selector[&fir.taps_count] > 0 && !blocks.contains(&fir.data_start) {
                return Err("missing FIR coefficient block".into());
            }
        }
    }
    Ok(())
}
