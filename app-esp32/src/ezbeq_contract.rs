//! Pure ezBEQ mutation planning and coefficient parsing shared with host tests.

pub fn parse_biquads(text: &str) -> Result<Vec<[f32; 5]>, String> {
    let mut result = Vec::new();
    let mut values = [None; 5];
    fn finish(values: &mut [Option<f32>; 5], result: &mut Vec<[f32; 5]>) -> Result<(), String> {
        if values.iter().any(Option::is_some) {
            if values.iter().any(Option::is_none) {
                return Err("each biquad needs b0, b1, b2, a1 and a2".into());
            }
            result.push(values.map(Option::unwrap));
            *values = [None; 5];
        }
        Ok(())
    }
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if line.starts_with("biquad") {
            finish(&mut values, &mut result)?;
        } else {
            let (key, raw) = line.split_once('=').ok_or("expected coefficient=value")?;
            let index = ["b0", "b1", "b2", "a1", "a2"]
                .iter()
                .position(|k| *k == key.trim())
                .ok_or_else(|| format!("unsupported coefficient {}", key.trim()))?;
            if values[index].is_some() {
                return Err(format!("duplicate coefficient {}", key.trim()));
            }
            let value = raw
                .trim()
                .trim_end_matches(',')
                .trim()
                .parse::<f32>()
                .map_err(|_| "invalid coefficient")?;
            if !value.is_finite() {
                return Err("coefficients must be finite".into());
            }
            values[index] = Some(value);
        }
    }
    finish(&mut values, &mut result)?;
    if result.len() > 10 {
        return Err("provide at most 10 biquads".into());
    }
    Ok(result)
}

/// Upstream __update_slot applies controls before loading/clearing an entry,
/// and activates on the presence of `active`, even when it is false.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotStep {
    Inputs,
    Outputs,
    Entry,
    Activate,
}

pub fn slot_steps(slot: &serde_json::Value) -> Result<Vec<SlotStep>, String> {
    if slot.get("active").is_some_and(|value| !value.is_boolean()) {
        return Err("active must be a boolean".into());
    }
    let mut steps = Vec::new();
    if ["gains", "mutes"].iter().any(|key| {
        slot.get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|v| !v.is_empty())
    }) {
        steps.push(SlotStep::Inputs);
    }
    if ["outputGains", "outputMutes"].iter().any(|key| {
        slot.get(key)
            .and_then(|v| v.as_array())
            .is_some_and(|v| !v.is_empty())
    }) {
        steps.push(SlotStep::Outputs);
    }
    if slot.get("entry").is_some() {
        steps.push(SlotStep::Entry);
    }
    if slot.get("active").is_some() {
        steps.push(SlotStep::Activate);
    }
    Ok(steps)
}

#[derive(Debug, PartialEq)]
pub enum BandAction {
    Write {
        index: usize,
        coefficients: [f32; 5],
    },
    Bypass {
        index: usize,
    },
}

/// Applied only to selected channels. Empty uploads bypass every selected
/// band when overwriting, and leave all bands untouched otherwise.
pub fn band_actions(biquads: &[[f32; 5]], overwrite: bool, bands: usize) -> Vec<BandAction> {
    (0..bands.min(10))
        .filter_map(|index| {
            if let Some(coefficients) = biquads.get(index) {
                Some(BandAction::Write {
                    index,
                    coefficients: *coefficients,
                })
            } else if overwrite {
                Some(BandAction::Bypass { index })
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_export_and_preserves_feedback_sign() {
        let text = "biquad1,\nb0=1,\nb1=0,\nb2=0,\na1=1.9,\na2=-0.95\n\nbiquad2,\nb0=1\nb1=0\nb2=0\na1=0\na2=0";
        assert_eq!(
            parse_biquads(text).unwrap(),
            vec![[1., 0., 0., 1.9, -0.95], [1., 0., 0., 0., 0.]]
        );
    }
    #[test]
    fn rejects_partial_duplicate_and_nonfinite() {
        for text in ["b0=1", "b0=1\nb0=2", "b0=NaN", "a0=1", "b0=inf"] {
            assert!(parse_biquads(text).is_err(), "{text}");
        }
    }
    #[test]
    fn rejects_excess_filters() {
        assert!(parse_biquads(&"biquad1\nb0=1\nb1=0\nb2=0\na1=0\na2=0\n".repeat(11)).is_err());
    }
}

#[derive(Debug)]
pub struct CatalogueEntry {
    pub coefficients: Vec<[f32; 5]>,
    pub mv: f32,
    pub optimised: bool,
    pub metadata_offset: Option<u32>,
}

/// BEQ v3 stores one coefficient representation and a pointer to its feed row.
/// v2 is accepted for installed bundles during the transition. Check the rate
/// before looking up an entry, and before any caller changes the DSP/preset.
pub fn read_entry<R: std::io::Read + std::io::Seek>(
    file: &mut R,
    target: &[u8; 8],
    expected_rate: Option<u32>,
) -> Result<CatalogueEntry, String> {
    use std::io::SeekFrom;
    fn read<const N: usize>(file: &mut impl std::io::Read) -> Result<[u8; N], String> {
        let mut buf = [0; N];
        file.read_exact(&mut buf).map_err(|e| e.to_string())?;
        Ok(buf)
    }
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let header = read::<12>(file)?;
    let legacy = &header[..4] == b"BEQ\x02";
    if !legacy && &header[..4] != b"BEQ\x03" {
        return Err("unsupported catalogue.bin format".into());
    }
    let rate = u32::from_le_bytes(header[8..12].try_into().unwrap());
    if let Some(expected) = expected_rate.filter(|&expected| rate != expected) {
        return Err(format!(
            "catalogue sample rate {rate} Hz differs from device {expected} Hz"
        ));
    }
    let count = u32::from_le_bytes(header[4..8].try_into().unwrap()) as u64;
    let (mut lo, mut hi) = (0, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        file.seek(SeekFrom::Start(12 + mid * if legacy { 213 } else { 12 }))
            .map_err(|e| e.to_string())?;
        let prefix = read::<8>(file)?;
        match prefix.cmp(target) {
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
            std::cmp::Ordering::Equal => {
                if !legacy {
                    let offset = u32::from_le_bytes(read::<4>(file)?) as u64;
                    if offset < 12 + count * 12 {
                        return Err("invalid catalogue record offset".into());
                    }
                    file.seek(SeekFrom::Start(offset))
                        .map_err(|e| e.to_string())?;
                }
                let header = read::<5>(file)?;
                let optimised = !legacy && header[0] & 0x80 != 0;
                let bands = if legacy { header[0] } else { header[0] & 0x7f };
                if bands > 10 {
                    return Err("invalid biquad count".into());
                }
                let mv = f32::from_le_bytes(header[1..5].try_into().unwrap());
                if !mv.is_finite() || mv.abs() > 40.0 {
                    return Err("invalid MV adjustment".into());
                }
                let mut coefficients = Vec::with_capacity(bands as usize);
                for _ in 0..bands {
                    let mut values = [0.0; 5];
                    for value in &mut values {
                        *value = f32::from_le_bytes(read::<4>(file)?);
                        if !value.is_finite() {
                            return Err("non-finite biquad coefficient".into());
                        }
                    }
                    coefficients.push(values);
                }
                let metadata_offset = if legacy {
                    None
                } else {
                    Some(u32::from_le_bytes(read::<4>(file)?))
                };
                return Ok(CatalogueEntry {
                    coefficients,
                    mv,
                    optimised,
                    metadata_offset,
                });
            }
        }
    }
    Err("entry not found in catalogue".into())
}

#[cfg(test)]
mod catalogue_tests {
    use super::*;
    use std::io::Cursor;

    fn catalogue() -> Vec<u8> {
        let mut bytes = b"BEQ\x03".to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(96000u32.to_le_bytes());
        bytes.extend([7; 8]);
        bytes.extend(24u32.to_le_bytes());
        bytes.push(0x81);
        bytes.extend((-3.0f32).to_le_bytes());
        for c in [1.0f32, 0.0, 0.0, 1.9, -0.95] {
            bytes.extend(c.to_le_bytes());
        }
        bytes.extend(123u32.to_le_bytes());
        bytes
    }

    #[test]
    fn reads_optimised_coefficients_and_metadata_without_sign_conversion() {
        let entry = read_entry(&mut Cursor::new(catalogue()), &[7; 8], Some(96000)).unwrap();
        assert_eq!(entry.coefficients, vec![[1.0, 0.0, 0.0, 1.9, -0.95]]);
        assert_eq!(entry.mv, -3.0);
        assert!(entry.optimised);
        assert_eq!(entry.metadata_offset, Some(123));
    }

    #[test]
    fn refuses_rate_mismatch_missing_entry_and_corrupt_records() {
        assert!(
            read_entry(&mut Cursor::new(catalogue()), &[7; 8], Some(48000))
                .unwrap_err()
                .contains("sample rate")
        );
        for key in [[0; 8], [8; 8]] {
            assert!(read_entry(&mut Cursor::new(catalogue()), &key, Some(96000)).is_err());
        }
        for length in [0, 11, 23, 24, 48] {
            let truncated = catalogue()[..length].to_vec();
            assert!(read_entry(&mut Cursor::new(truncated), &[7; 8], None).is_err());
        }
        for (offset, value) in [
            (20, 0u32),
            (25, f32::NAN.to_bits()),
            (29, f32::INFINITY.to_bits()),
        ] {
            let mut bytes = catalogue();
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            assert!(read_entry(&mut Cursor::new(bytes), &[7; 8], None).is_err());
        }
    }

    #[test]
    fn accepts_authored_legacy_bundles_but_still_checks_rate() {
        let mut bytes = b"BEQ\x02".to_vec();
        bytes.extend(1u32.to_le_bytes());
        bytes.extend(96000u32.to_le_bytes());
        bytes.extend([7; 8]);
        bytes.push(1);
        bytes.extend(0f32.to_le_bytes());
        for c in [1f32, 0., 0., 0., 0.] {
            bytes.extend(c.to_le_bytes());
        }
        bytes.resize(225, 0);
        let entry = read_entry(&mut Cursor::new(&bytes), &[7; 8], Some(96000)).unwrap();
        assert!(!entry.optimised);
        assert_eq!(entry.metadata_offset, None);
        assert!(read_entry(&mut Cursor::new(bytes), &[7; 8], Some(48000)).is_err());
    }
}
