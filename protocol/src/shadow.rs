//! Pure shadow defaults and raw-write decoding shared by both device clients.
//!
//! These parsers mirror successful raw writes; they are not general command
//! validators. Their accepted payload shapes intentionally match both clients.

use crate::commands;
use crate::device::DeviceProfile;
use crate::dialect::{Dialect, ValueFormat};
use std::collections::HashMap;

/// Coefficients and bypass state for a PEQ or crossover biquad.
#[derive(Debug, Clone, Copy)]
pub struct PeqShadow {
    pub coeffs: [f32; 5],
    pub bypass: bool,
}

/// Seed all profile PEQ and crossover biquads with identity coefficients.
pub fn build_peq_shadow(profile: &DeviceProfile) -> HashMap<u16, PeqShadow> {
    let mut shadow = HashMap::new();
    if let Some(table) = profile.dsp_table {
        for input in table.inputs {
            for band in input.peq {
                shadow.insert(
                    band.addr,
                    PeqShadow {
                        coeffs: crate::biquad::BYPASS,
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
                        coeffs: crate::biquad::BYPASS,
                        bypass: false,
                    },
                );
            }
            if let Some(crossover) = output.crossover {
                for base in [crossover.lpf_addr, crossover.hpf_addr] {
                    for i in 0..4u16 {
                        shadow.insert(
                            base + i * 5,
                            PeqShadow {
                                coeffs: crate::biquad::BYPASS,
                                bypass: false,
                            },
                        );
                    }
                }
            }
        }
    }
    shadow
}

/// Return a zero fallback for placeholder or invalid scalar readback.
/// `None` leaves the caller to use its existing readback/shadow policy.
pub fn sanitize_shadow_preferred_scalar(raw: u32) -> Option<f32> {
    if raw <= 2 {
        return Some(0.0);
    }

    let value = f32::from_bits(raw);
    if !value.is_finite() || value.abs() > 10_000.0 {
        return Some(0.0);
    }

    None
}

/// Decode the primary coefficients of an unlinked raw biquad write.
pub fn parse_raw_write_biquad(payload: &[u8], dialect: &Dialect) -> Option<(u16, [f32; 5])> {
    if payload.first().copied()? != commands::CMD_WRITE_BIQUAD || payload.len() < 26 {
        return None;
    }
    if payload[1] != 0x80 {
        return None;
    }
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    let mut coeffs = [0.0f32; 5];
    let mut offset = 6;
    for coeff in &mut coeffs {
        let bytes: [u8; 4] = payload.get(offset..offset + 4)?.try_into().ok()?;
        *coeff = crate::dialect::decode_value(&bytes, dialect);
        offset += 4;
    }
    Some((addr, coeffs))
}

/// Decode the primary address and bypass flag of a raw filter write.
pub fn parse_raw_bypass_filter(payload: &[u8]) -> Option<(u16, bool)> {
    if payload.first().copied()? != commands::CMD_BYPASS_FILTER || payload.len() < 6 {
        return None;
    }
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    Some((addr, payload[1] & 0x80 != 0))
}

/// Decode a persisted SHARC selector write with a padded 16-bit value.
pub fn parse_raw_selector_write(payload: &[u8], dialect: &Dialect) -> Option<(u16, u16)> {
    if dialect.value_format != ValueFormat::Float32 {
        return None;
    }
    if payload.first().copied()? != commands::CMD_WRITE_DSP || payload.len() < 8 {
        return None;
    }
    if payload[1] != 0x80 || payload[6] != 0 || payload[7] != 0 {
        return None;
    }
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    let value = u16::from_le_bytes([payload[4], payload[5]]);
    Some((addr, value))
}

/// Decode the primary address and low 16 value bits of a SHARC linked write.
pub fn parse_raw_linked_selector_write(payload: &[u8], dialect: &Dialect) -> Option<(u16, u16)> {
    if dialect.value_format != ValueFormat::Float32 {
        return None;
    }
    if payload.first().copied()? != commands::CMD_WRITE_BIQUAD || payload.len() < 10 {
        return None;
    }
    let ctrl = payload[1];
    if ctrl < 0x80 {
        return None;
    }
    let linked_count = (ctrl - 0x80) as usize;
    let linked_bytes = if linked_count == 0 {
        2
    } else {
        linked_count * 2
    };
    let value_offset = 4 + linked_bytes;
    let value_bytes: [u8; 4] = payload
        .get(value_offset..value_offset + 4)?
        .try_into()
        .ok()?;
    let addr = u16::from_be_bytes([payload[2], payload[3]]);
    Some((addr, u32::from_le_bytes(value_bytes) as u16))
}
